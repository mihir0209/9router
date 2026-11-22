use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::db::{Database, ProviderConnection};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default buffer before token expiry to trigger background refresh (5 minutes).
pub const DEFAULT_EXPIRY_BUFFER_SECS: i64 = 5 * 60;

/// Default delay in milliseconds between refreshing consecutive connections (1500ms).
pub const DEFAULT_BG_REFRESH_DELAY_MS: u64 = 1500;

/// Default delay in milliseconds for sensitive providers like Google / Antigravity (12000ms).
pub const DEFAULT_BG_REFRESH_GOOGLE_DELAY_MS: u64 = 12000;

/// Default periodic background scan interval (5 minutes = 300 seconds).
pub const DEFAULT_SCAN_INTERVAL_SECS: u64 = 300;

/// Default upstream model catalog API endpoint.
pub const DEFAULT_MODEL_CATALOG_URL: &str = "https://models.dev/api.json";

/// Providers that require conservative rate-limit spacing to prevent upstream quota exhaustion.
pub const SENSITIVE_PROVIDERS: &[&str] = &["google", "gemini", "antigravity", "gemini-cli"];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for background token refresh and model catalog synchronization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRefreshConfig {
    /// Kill-switch: when true, background token refresh is completely disabled.
    /// Loaded from `DISABLE_BACKGROUND_TOKEN_REFRESH` (default: false / "0").
    pub disabled: bool,

    /// Pacing delay between refreshing consecutive accounts in milliseconds.
    /// Loaded from `BG_REFRESH_DELAY_MS` (default: 1500ms).
    pub delay_ms: u64,

    /// Pacing delay for sensitive providers (e.g. Google Cloud) in milliseconds.
    /// Loaded from `BG_REFRESH_GOOGLE_DELAY_MS` (default: 12000ms).
    pub google_delay_ms: u64,

    /// Periodic scan interval in seconds (default: 300s).
    pub scan_interval_secs: u64,

    /// Expiry lookahead window in seconds (default: 300s / 5 minutes).
    /// Identifies OAuth tokens expiring within this duration.
    pub expiry_buffer_secs: i64,

    /// Whether periodic model catalog sync is active.
    /// Loaded from `MODEL_CATALOG_SYNC` (default: true / "on" / "1").
    pub model_catalog_sync: bool,

    /// Upstream model catalog URL.
    pub catalog_url: String,
}

impl Default for TokenRefreshConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            delay_ms: DEFAULT_BG_REFRESH_DELAY_MS,
            google_delay_ms: DEFAULT_BG_REFRESH_GOOGLE_DELAY_MS,
            scan_interval_secs: DEFAULT_SCAN_INTERVAL_SECS,
            expiry_buffer_secs: DEFAULT_EXPIRY_BUFFER_SECS,
            model_catalog_sync: true,
            catalog_url: DEFAULT_MODEL_CATALOG_URL.to_string(),
        }
    }
}

impl TokenRefreshConfig {
    /// Load configuration from process environment variables.
    pub fn load() -> Self {
        Self::load_from(|k| std::env::var(k).ok())
    }

    /// Load configuration using a custom key-value lookup function (for testing).
    pub fn load_from<F: Fn(&str) -> Option<String>>(lookup: F) -> Self {
        let disabled = match lookup("DISABLE_BACKGROUND_TOKEN_REFRESH") {
            Some(v) => is_truthy(&v),
            None => false,
        };

        let delay_ms = lookup("BG_REFRESH_DELAY_MS")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_BG_REFRESH_DELAY_MS);

        let google_delay_ms = lookup("BG_REFRESH_GOOGLE_DELAY_MS")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_BG_REFRESH_GOOGLE_DELAY_MS);

        let scan_interval_secs = lookup("TOKEN_REFRESH_SCAN_INTERVAL_SECS")
            .or_else(|| lookup("BG_REFRESH_INTERVAL_SECS"))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_SCAN_INTERVAL_SECS);

        let expiry_buffer_secs = lookup("TOKEN_EXPIRY_THRESHOLD_SECS")
            .or_else(|| lookup("TOKEN_EXPIRY_BUFFER_SECS"))
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_EXPIRY_BUFFER_SECS);

        let model_catalog_sync = match lookup("MODEL_CATALOG_SYNC") {
            Some(v) => {
                let s = v.trim().to_ascii_lowercase();
                !(s == "off" || s == "0" || s == "false" || s == "no")
            }
            None => true,
        };

        let catalog_url = lookup("MODEL_CATALOG_URL")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL_CATALOG_URL.to_string());

        Self {
            disabled,
            delay_ms,
            google_delay_ms,
            scan_interval_secs,
            expiry_buffer_secs,
            model_catalog_sync,
            catalog_url,
        }
    }
}

/// Parses truthy string values like "1", "true", "yes", "on".
pub fn is_truthy(val: &str) -> bool {
    matches!(
        val.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

// ---------------------------------------------------------------------------
// Candidate Representation & Token Parsing
// ---------------------------------------------------------------------------

/// Information on a provider connection token identified as expiring or expired.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiringTokenCandidate {
    pub connection_id: String,
    pub provider: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub auth_type: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub seconds_until_expiry: Option<i64>,
    pub is_expired: bool,
    pub refresh_token: String,
}

/// Checks if an auth type is OAuth.
pub fn is_oauth_connection(conn: &ProviderConnection) -> bool {
    let t = conn.auth_type.trim().to_ascii_lowercase().replace('_', "");
    t == "oauth" || t == "oauth2"
}

/// Extracts the OAuth refresh token from a provider connection.
pub fn extract_refresh_token(conn: &ProviderConnection) -> Option<String> {
    // 1. Check parsed extra map
    for key in &["refreshToken", "refresh_token"] {
        if let Some(val) = conn.data.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // 2. Check nested providerSpecificData in extra
    if let Some(psd) = conn.data.extra.get("providerSpecificData").and_then(|v| v.as_object()) {
        for key in &["refreshToken", "refresh_token"] {
            if let Some(s) = psd.get(*key).and_then(|v| v.as_str()) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    // 3. Fallback to raw_data JSON parsing
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&conn.raw_data) {
        for key in &["refreshToken", "refresh_token"] {
            if let Some(s) = v.get(*key).and_then(|x| x.as_str()) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        if let Some(psd) = v.get("providerSpecificData").and_then(|x| x.as_object()) {
            for key in &["refreshToken", "refresh_token"] {
                if let Some(s) = psd.get(*key).and_then(|x| x.as_str()) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    None
}

/// Extracts the access token from a provider connection.
pub fn extract_access_token(conn: &ProviderConnection) -> Option<String> {
    if let Some(k) = &conn.data.api_key {
        let trimmed = k.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    for key in &["accessToken", "access_token"] {
        if let Some(val) = conn.data.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&conn.raw_data) {
        for key in &["accessToken", "access_token"] {
            if let Some(s) = v.get(*key).and_then(|x| x.as_str()) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    None
}

/// Parses an expiry timestamp from various potential representations:
/// - RFC 3339 / ISO 8601 string ("2026-11-22T17:15:00Z")
/// - Numeric timestamp in milliseconds (> 1e12) or seconds
/// - Numeric string ("1700000000")
/// - Relative expiration ("expiresIn")
pub fn extract_token_expiry(conn: &ProviderConnection) -> Option<DateTime<Utc>> {
    let check_keys = ["expiresAt", "tokenExpiresAt", "expires_at"];

    // 1. Check data.extra
    for key in &check_keys {
        if let Some(val) = conn.data.extra.get(*key) {
            if let Some(dt) = parse_json_timestamp(val) {
                return Some(dt);
            }
        }
    }

    // 2. Check providerSpecificData
    if let Some(psd) = conn.data.extra.get("providerSpecificData").and_then(|v| v.as_object()) {
        for key in &check_keys {
            if let Some(val) = psd.get(*key) {
                if let Some(dt) = parse_json_timestamp(val) {
                    return Some(dt);
                }
            }
        }
    }

    // 3. Fallback to raw_data
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&conn.raw_data) {
        for key in &check_keys {
            if let Some(val) = v.get(*key) {
                if let Some(dt) = parse_json_timestamp(val) {
                    return Some(dt);
                }
            }
        }
        if let Some(psd) = v.get("providerSpecificData").and_then(|x| x.as_object()) {
            for key in &check_keys {
                if let Some(val) = psd.get(*key) {
                    if let Some(dt) = parse_json_timestamp(val) {
                        return Some(dt);
                    }
                }
            }
        }

        // Relative expiresIn check
        for key in &["expiresIn", "expires_in"] {
            if let Some(seconds) = v.get(*key).and_then(|x| x.as_i64()) {
                if seconds > 0 {
                    let base_time = DateTime::parse_from_rfc3339(&conn.updated_at)
                        .map(|dt| dt.with_timezone(&Utc))
                        .or_else(|_| {
                            DateTime::parse_from_rfc3339(&conn.created_at)
                                .map(|dt| dt.with_timezone(&Utc))
                        })
                        .ok();
                    if let Some(base) = base_time {
                        return Some(base + chrono::Duration::seconds(seconds));
                    }
                }
            }
        }
    }

    None
}

/// Parses a serde_json::Value representing a timestamp into DateTime<Utc>.
fn parse_json_timestamp(val: &serde_json::Value) -> Option<DateTime<Utc>> {
    match val {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Try RFC 3339
            if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
                return Some(dt.with_timezone(&Utc));
            }
            // Try numeric in string
            if let Ok(num) = trimmed.parse::<i64>() {
                return parse_numeric_timestamp(num);
            }
            // Try common non-RFC3339 patterns
            if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
                return Some(naive.and_utc());
            }
            None
        }
        serde_json::Value::Number(num) => {
            if let Some(i) = num.as_i64() {
                parse_numeric_timestamp(i)
            } else if let Some(f) = num.as_f64() {
                parse_numeric_timestamp(f as i64)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_numeric_timestamp(num: i64) -> Option<DateTime<Utc>> {
    if num <= 0 {
        return None;
    }
    if num > 1_000_000_000_000 {
        // Milliseconds
        DateTime::from_timestamp_millis(num)
    } else {
        // Seconds
        DateTime::from_timestamp(num, 0)
    }
}

/// Determines whether a provider connection token is expiring within the specified buffer window.
pub fn is_token_expiring_within(
    conn: &ProviderConnection,
    now: DateTime<Utc>,
    buffer_seconds: i64,
) -> bool {
    if !conn.is_active {
        return false;
    }
    if !is_oauth_connection(conn) {
        return false;
    }
    if extract_refresh_token(conn).is_none() {
        return false;
    }

    match extract_token_expiry(conn) {
        Some(expiry) => expiry <= now + chrono::Duration::seconds(buffer_seconds),
        None => {
            // OAuth connection with a refresh token but without explicit expiry timestamp
            // is flagged to ensure token freshness.
            true
        }
    }
}

/// Identifies all provider connections having OAuth tokens expiring within `buffer_seconds`.
pub fn identify_expiring_tokens(
    connections: &[ProviderConnection],
    now: DateTime<Utc>,
    buffer_seconds: i64,
) -> Vec<ExpiringTokenCandidate> {
    let mut candidates = Vec::new();
    let lead_duration = chrono::Duration::seconds(buffer_seconds);

    for conn in connections {
        if !conn.is_active {
            continue;
        }
        if !is_oauth_connection(conn) {
            continue;
        }

        let refresh_token = match extract_refresh_token(conn) {
            Some(rt) => rt,
            None => continue,
        };

        let expiry = extract_token_expiry(conn);
        match expiry {
            Some(exp) => {
                let diff_seconds = (exp - now).num_seconds();
                if exp <= now + lead_duration {
                    candidates.push(ExpiringTokenCandidate {
                        connection_id: conn.id.clone(),
                        provider: conn.provider.clone(),
                        name: conn.name.clone(),
                        email: conn.email.clone(),
                        auth_type: conn.auth_type.clone(),
                        expires_at: Some(exp),
                        seconds_until_expiry: Some(diff_seconds),
                        is_expired: exp <= now,
                        refresh_token,
                    });
                }
            }
            None => {
                candidates.push(ExpiringTokenCandidate {
                    connection_id: conn.id.clone(),
                    provider: conn.provider.clone(),
                    name: conn.name.clone(),
                    email: conn.email.clone(),
                    auth_type: conn.auth_type.clone(),
                    expires_at: None,
                    seconds_until_expiry: None,
                    is_expired: false,
                    refresh_token,
                });
            }
        }
    }

    candidates
}

/// Returns references to connections needing token refresh.
pub fn select_connections_needing_refresh<'a>(
    connections: &'a [ProviderConnection],
    now: DateTime<Utc>,
    buffer_seconds: i64,
) -> Vec<&'a ProviderConnection> {
    connections
        .iter()
        .filter(|c| is_token_expiring_within(c, now, buffer_seconds))
        .collect()
}

/// Returns the pacing delay in milliseconds for a provider.
pub fn get_pacing_delay_ms(provider: &str, config: &TokenRefreshConfig) -> u64 {
    let p = provider.trim().to_ascii_lowercase();
    if SENSITIVE_PROVIDERS.iter().any(|&s| s == p) {
        config.google_delay_ms
    } else {
        config.delay_ms
    }
}

// ---------------------------------------------------------------------------
// Token Refresh Execution & Database Persistence
// ---------------------------------------------------------------------------

/// Result of a token refresh attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshResult {
    pub connection_id: String,
    pub provider: String,
    pub success: bool,
    pub new_access_token: Option<String>,
    pub new_refresh_token: Option<String>,
    pub new_expires_at: Option<String>,
    pub error: Option<String>,
}

/// Updates provider credentials in the SQLite database after a successful refresh.
pub fn update_connection_tokens(
    db: &Database,
    conn_id: &str,
    new_access_token: &str,
    new_refresh_token: Option<&str>,
    new_expires_at: Option<&str>,
) -> anyhow::Result<ProviderConnection> {
    let mut connections = db.load_provider_connections()?;
    let conn_idx = connections
        .iter()
        .position(|c| c.id == conn_id)
        .ok_or_else(|| anyhow::anyhow!("Connection '{}' not found", conn_id))?;

    let mut conn = connections.remove(conn_idx);

    // Update ConnectionData fields
    conn.data.api_key = Some(new_access_token.to_string());
    conn.data.extra.insert("accessToken".to_string(), json!(new_access_token));

    if let Some(rt) = new_refresh_token {
        conn.data.extra.insert("refreshToken".to_string(), json!(rt));
    }
    if let Some(exp) = new_expires_at {
        conn.data.extra.insert("expiresAt".to_string(), json!(exp));
    }

    // Synchronize raw_data JSON
    let mut raw_val: serde_json::Value = serde_json::from_str(&conn.raw_data)
        .unwrap_or_else(|_| serde_json::to_value(&conn.data).unwrap_or(json!({})));

    if let Some(obj) = raw_val.as_object_mut() {
        obj.insert("apiKey".to_string(), json!(new_access_token));
        obj.insert("accessToken".to_string(), json!(new_access_token));
        if let Some(rt) = new_refresh_token {
            obj.insert("refreshToken".to_string(), json!(rt));
        }
        if let Some(exp) = new_expires_at {
            obj.insert("expiresAt".to_string(), json!(exp));
        }
    }
    conn.raw_data = serde_json::to_string(&raw_val)?;
    conn.updated_at = Utc::now().to_rfc3339();

    db.save_provider_connection(&conn)?;
    Ok(conn)
}

/// Attempts to refresh the OAuth token for a given provider connection.
/// Fail-open: errors are logged and captured in `RefreshResult`, never panicking.
pub async fn refresh_oauth_token(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    db: &Database,
) -> RefreshResult {
    let refresh_token = match extract_refresh_token(conn) {
        Some(rt) => rt,
        None => {
            return RefreshResult {
                connection_id: conn.id.clone(),
                provider: conn.provider.clone(),
                success: false,
                new_access_token: None,
                new_refresh_token: None,
                new_expires_at: None,
                error: Some("No refresh token available on connection".to_string()),
            };
        }
    };

    let p = conn.provider.trim().to_ascii_lowercase();

    // Check custom token endpoint or known provider endpoints
    let token_endpoint = conn
        .data
        .extra
        .get("tokenEndpoint")
        .or_else(|| conn.data.extra.get("token_endpoint"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            if p == "google" || p == "gemini" || p == "gemini-cli" {
                Some("https://oauth2.googleapis.com/token".to_string())
            } else if p == "claude" || p == "anthropic" {
                Some("https://api.anthropic.com/v1/oauth/token".to_string())
            } else {
                None
            }
        });

    let endpoint = match token_endpoint {
        Some(ep) => ep,
        None => {
            return RefreshResult {
                connection_id: conn.id.clone(),
                provider: conn.provider.clone(),
                success: false,
                new_access_token: None,
                new_refresh_token: None,
                new_expires_at: None,
                error: Some(format!("No OAuth token refresh endpoint resolved for provider '{}'", conn.provider)),
            };
        }
    };

    let mut form_params: HashMap<&str, String> = HashMap::new();
    form_params.insert("grant_type", "refresh_token".to_string());
    form_params.insert("refresh_token", refresh_token.clone());

    if let Some(cid) = conn.data.extra.get("clientId").and_then(|v| v.as_str()) {
        form_params.insert("client_id", cid.to_string());
    }
    if let Some(csec) = conn.data.extra.get("clientSecret").and_then(|v| v.as_str()) {
        form_params.insert("client_secret", csec.to_string());
    }

    match client
        .post(&endpoint)
        .form(&form_params)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let new_acc = body
                        .get("access_token")
                        .or_else(|| body.get("accessToken"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let new_ref = body
                        .get("refresh_token")
                        .or_else(|| body.get("refreshToken"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let expires_in = body
                        .get("expires_in")
                        .or_else(|| body.get("expiresIn"))
                        .and_then(|v| v.as_i64());

                    let new_exp = expires_in.map(|secs| {
                        (Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
                    });

                    if let Some(acc) = &new_acc {
                        if let Err(e) = update_connection_tokens(
                            db,
                            &conn.id,
                            acc,
                            new_ref.as_deref(),
                            new_exp.as_deref(),
                        ) {
                            warn!(
                                id = %conn.id,
                                provider = %conn.provider,
                                "Failed to update refreshed tokens in database: {e}"
                            );
                        } else {
                            info!(
                                id = %conn.id,
                                provider = %conn.provider,
                                "Successfully refreshed OAuth token in background"
                            );
                        }
                    }

                    RefreshResult {
                        connection_id: conn.id.clone(),
                        provider: conn.provider.clone(),
                        success: new_acc.is_some(),
                        new_access_token: new_acc,
                        new_refresh_token: new_ref,
                        new_expires_at: new_exp,
                        error: None,
                    }
                } else {
                    RefreshResult {
                        connection_id: conn.id.clone(),
                        provider: conn.provider.clone(),
                        success: false,
                        new_access_token: None,
                        new_refresh_token: None,
                        new_expires_at: None,
                        error: Some("Failed to parse token response JSON".to_string()),
                    }
                }
            } else {
                let err_msg = format!("Upstream OAuth endpoint returned HTTP {}", status);
                warn!(
                    id = %conn.id,
                    provider = %conn.provider,
                    "Background token refresh failed (swallowed): {err_msg}"
                );
                RefreshResult {
                    connection_id: conn.id.clone(),
                    provider: conn.provider.clone(),
                    success: false,
                    new_access_token: None,
                    new_refresh_token: None,
                    new_expires_at: None,
                    error: Some(err_msg),
                }
            }
        }
        Err(e) => {
            let err_msg = e.to_string();
            warn!(
                id = %conn.id,
                provider = %conn.provider,
                "Background token refresh network request failed (swallowed): {err_msg}"
            );
            RefreshResult {
                connection_id: conn.id.clone(),
                provider: conn.provider.clone(),
                success: false,
                new_access_token: None,
                new_refresh_token: None,
                new_expires_at: None,
                error: Some(err_msg),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Model Catalog Synchronization
// ---------------------------------------------------------------------------

/// Result of an upstream model catalog synchronization attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogSyncResult {
    pub status: String, // "updated", "unchanged", "skipped", "error"
    pub models_count: usize,
    pub providers_count: usize,
    pub etag: Option<String>,
    pub file_path: Option<String>,
    pub error: Option<String>,
}

/// Synchronizes upstream model catalog (e.g. from models.dev) and writes to disk.
/// Fail-open: network or parsing errors are logged as warnings and swallowed.
pub async fn sync_model_catalog(
    client: &reqwest::Client,
    data_dir: &Path,
    config: &TokenRefreshConfig,
    current_etag: Option<&str>,
) -> ModelCatalogSyncResult {
    if !config.model_catalog_sync {
        debug!("Model catalog sync skipped (disabled via MODEL_CATALOG_SYNC)");
        return ModelCatalogSyncResult {
            status: "skipped".to_string(),
            models_count: 0,
            providers_count: 0,
            etag: None,
            file_path: None,
            error: None,
        };
    }

    let mut request = client
        .get(&config.catalog_url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30));

    if let Some(etag) = current_etag {
        request = request.header("If-None-Match", etag);
    }

    match request.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status == reqwest::StatusCode::NOT_MODIFIED {
                debug!("Upstream model catalog unchanged (HTTP 304)");
                return ModelCatalogSyncResult {
                    status: "unchanged".to_string(),
                    models_count: 0,
                    providers_count: 0,
                    etag: current_etag.map(|s| s.to_string()),
                    file_path: None,
                    error: None,
                };
            }

            if !status.is_success() {
                let err_msg = format!("Upstream model catalog returned HTTP {}", status);
                warn!("Model catalog sync failed (swallowed): {err_msg}");
                return ModelCatalogSyncResult {
                    status: "error".to_string(),
                    models_count: 0,
                    providers_count: 0,
                    etag: None,
                    file_path: None,
                    error: Some(err_msg),
                };
            }

            let new_etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            match resp.json::<serde_json::Value>().await {
                Ok(catalog_json) => {
                    let mut providers_count = 0;
                    let mut models_count = 0;

                    if let Some(obj) = catalog_json.as_object() {
                        providers_count = obj.len();
                        for (_prov, val) in obj {
                            if let Some(models) = val.get("models").and_then(|m| m.as_object()) {
                                models_count += models.len();
                            }
                        }
                    }

                    // Write atomically to catalog file
                    let catalog_path = data_dir.join("models_catalog.json");
                    let temp_path = data_dir.join("models_catalog.json.tmp");

                    if let Err(e) = std::fs::create_dir_all(data_dir) {
                        warn!("Failed to create data directory for model catalog: {e}");
                    }

                    let write_res = serde_json::to_string(&catalog_json)
                        .map_err(anyhow::Error::from)
                        .and_then(|serialized| {
                            std::fs::write(&temp_path, serialized)?;
                            std::fs::rename(&temp_path, &catalog_path)?;
                            Ok(())
                        });

                    if let Err(e) = write_res {
                        warn!("Failed to write model catalog to disk (swallowed): {e}");
                    } else {
                        info!(
                            models = models_count,
                            providers = providers_count,
                            "Model catalog synchronized successfully"
                        );
                    }

                    ModelCatalogSyncResult {
                        status: "updated".to_string(),
                        models_count,
                        providers_count,
                        etag: new_etag,
                        file_path: Some(catalog_path.display().to_string()),
                        error: None,
                    }
                }
                Err(e) => {
                    let err_msg = format!("Failed to parse model catalog JSON: {e}");
                    warn!("Model catalog sync failed (swallowed): {err_msg}");
                    ModelCatalogSyncResult {
                        status: "error".to_string(),
                        models_count: 0,
                        providers_count: 0,
                        etag: None,
                        file_path: None,
                        error: Some(err_msg),
                    }
                }
            }
        }
        Err(e) => {
            let err_msg = format!("Model catalog HTTP request error: {e}");
            warn!("Model catalog sync failed (swallowed): {err_msg}");
            ModelCatalogSyncResult {
                status: "error".to_string(),
                models_count: 0,
                providers_count: 0,
                etag: None,
                file_path: None,
                error: Some(err_msg),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Background Worker Task
// ---------------------------------------------------------------------------

/// Worker managing background token lifecycle and catalog sync.
pub struct TokenRefreshWorker {
    pub config: TokenRefreshConfig,
    pub db: Arc<Database>,
    pub data_dir: PathBuf,
    pub client: reqwest::Client,
    pub shutdown: Arc<Notify>,
}

impl TokenRefreshWorker {
    pub async fn run(self) {
        if self.config.disabled {
            info!("Background token refresh is disabled via DISABLE_BACKGROUND_TOKEN_REFRESH");
            return;
        }

        info!(
            scan_interval_secs = self.config.scan_interval_secs,
            delay_ms = self.config.delay_ms,
            google_delay_ms = self.config.google_delay_ms,
            model_catalog_sync = self.config.model_catalog_sync,
            "Background token refresh worker started"
        );

        // Initial delay before first tick so initial gateway startup is zero-overhead
        let initial_delay = Duration::from_millis(std::cmp::min(self.config.delay_ms, 1500));
        tokio::select! {
            _ = tokio::time::sleep(initial_delay) => {},
            _ = self.shutdown.notified() => {
                info!("Background token refresh worker terminated during startup delay");
                return;
            }
        }

        let mut etag: Option<String> = None;
        let scan_interval = Duration::from_secs(std::cmp::max(self.config.scan_interval_secs, 1));

        loop {
            // 1. Run token refresh scan
            self.run_token_refresh_tick().await;

            // 2. Run model catalog sync if enabled
            if self.config.model_catalog_sync {
                let cat_res = sync_model_catalog(
                    &self.client,
                    &self.data_dir,
                    &self.config,
                    etag.as_deref(),
                )
                .await;
                if cat_res.status == "updated" && cat_res.etag.is_some() {
                    etag = cat_res.etag;
                }
            }

            // 3. Sleep until next scan interval or shutdown signal
            tokio::select! {
                _ = tokio::time::sleep(scan_interval) => {},
                _ = self.shutdown.notified() => {
                    info!("Background token refresh worker received shutdown signal");
                    break;
                }
            }
        }
    }

    /// Executes one scan tick: loads connections, finds expiring tokens, refreshes them with pacing.
    pub async fn run_token_refresh_tick(&self) -> Vec<RefreshResult> {
        let mut results = Vec::new();
        let connections = match self.db.load_provider_connections() {
            Ok(c) => c,
            Err(e) => {
                warn!("Background token refresh failed to load provider connections: {e}");
                return results;
            }
        };

        let now = Utc::now();
        let expiring = identify_expiring_tokens(&connections, now, self.config.expiry_buffer_secs);

        if expiring.is_empty() {
            debug!("No provider connection tokens require background refresh");
            return results;
        }

        info!(
            count = expiring.len(),
            "Found provider connection(s) requiring token refresh"
        );

        for (i, candidate) in expiring.iter().enumerate() {
            if let Some(conn) = connections.iter().find(|c| c.id == candidate.connection_id) {
                let res = refresh_oauth_token(&self.client, conn, &self.db).await;
                results.push(res);

                // Sequential delay between consecutive accounts to avoid upstream rate limits
                if i < expiring.len() - 1 {
                    let delay = get_pacing_delay_ms(&conn.provider, &self.config);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }

        results
    }
}

/// Handle to a running background token refresh worker.
pub struct TokenRefreshHandle {
    pub shutdown: Arc<Notify>,
    pub join_handle: tokio::task::JoinHandle<()>,
}

impl TokenRefreshHandle {
    /// Signals the background worker to stop and wait for clean termination.
    pub fn stop(&self) {
        self.shutdown.notify_waiters();
    }

    /// Awaits the termination of the background worker task.
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.join_handle.await
    }
}

/// Spawns the background token refresh worker task.
/// Returns None if disabled via `DISABLE_BACKGROUND_TOKEN_REFRESH`.
pub fn start_background_token_refresh(
    config: TokenRefreshConfig,
    db: Arc<Database>,
    data_dir: &Path,
) -> Option<TokenRefreshHandle> {
    if config.disabled {
        info!("Background token refresh is disabled via DISABLE_BACKGROUND_TOKEN_REFRESH");
        return None;
    }

    let shutdown = Arc::new(Notify::new());
    let worker = TokenRefreshWorker {
        config,
        db,
        data_dir: data_dir.to_path_buf(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default(),
        shutdown: Arc::clone(&shutdown),
    };

    let join_handle = tokio::spawn(async move {
        worker.run().await;
    });

    Some(TokenRefreshHandle {
        shutdown,
        join_handle,
    })
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::db::ConnectionData;

    fn mock_connection(
        id: &str,
        provider: &str,
        auth_type: &str,
        is_active: bool,
        refresh_token: Option<&str>,
        expires_at: Option<&str>,
    ) -> ProviderConnection {
        let mut extra = HashMap::new();
        if let Some(rt) = refresh_token {
            extra.insert("refreshToken".to_string(), json!(rt));
        }
        if let Some(exp) = expires_at {
            extra.insert("expiresAt".to_string(), json!(exp));
        }

        let data = ConnectionData {
            api_key: Some("current-access-token".to_string()),
            base_url: None,
            headers: None,
            default_model: None,
            extra,
        };

        let raw_data = serde_json::to_string(&data).unwrap();

        ProviderConnection {
            id: id.to_string(),
            provider: provider.to_string(),
            auth_type: auth_type.to_string(),
            name: Some(format!("Test {}", provider)),
            email: Some(format!("test@{}", provider)),
            priority: Some(1),
            is_active,
            data,
            raw_data,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_token_refresh_config_defaults() {
        let cfg = TokenRefreshConfig::default();
        assert!(!cfg.disabled);
        assert_eq!(cfg.delay_ms, 1500);
        assert_eq!(cfg.google_delay_ms, 12000);
        assert_eq!(cfg.scan_interval_secs, 300);
        assert_eq!(cfg.expiry_buffer_secs, 300);
        assert!(cfg.model_catalog_sync);
        assert_eq!(cfg.catalog_url, "https://models.dev/api.json");
    }

    #[test]
    fn test_token_refresh_config_env_parsing() {
        let mut env_map = HashMap::new();
        env_map.insert("DISABLE_BACKGROUND_TOKEN_REFRESH".to_string(), "1".to_string());
        env_map.insert("BG_REFRESH_DELAY_MS".to_string(), "2500".to_string());
        env_map.insert("BG_REFRESH_GOOGLE_DELAY_MS".to_string(), "15000".to_string());
        env_map.insert("TOKEN_REFRESH_SCAN_INTERVAL_SECS".to_string(), "60".to_string());
        env_map.insert("TOKEN_EXPIRY_THRESHOLD_SECS".to_string(), "600".to_string());
        env_map.insert("MODEL_CATALOG_SYNC".to_string(), "off".to_string());
        env_map.insert("MODEL_CATALOG_URL".to_string(), "https://example.com/catalog.json".to_string());

        let cfg = TokenRefreshConfig::load_from(|k| env_map.get(k).cloned());
        assert!(cfg.disabled);
        assert_eq!(cfg.delay_ms, 2500);
        assert_eq!(cfg.google_delay_ms, 15000);
        assert_eq!(cfg.scan_interval_secs, 60);
        assert_eq!(cfg.expiry_buffer_secs, 600);
        assert!(!cfg.model_catalog_sync);
        assert_eq!(cfg.catalog_url, "https://example.com/catalog.json");
    }

    #[test]
    fn test_extract_tokens_and_expiry_formats() {
        // RFC 3339 string
        let conn1 = mock_connection(
            "c1",
            "google",
            "oauth",
            true,
            Some("rt_google_123"),
            Some("2026-11-22T18:00:00Z"),
        );
        assert_eq!(extract_refresh_token(&conn1), Some("rt_google_123".to_string()));
        assert_eq!(extract_access_token(&conn1), Some("current-access-token".to_string()));
        let exp1 = extract_token_expiry(&conn1).expect("parsed expiry");
        assert_eq!(exp1.to_rfc3339(), "2026-11-22T18:00:00+00:00");

        // Numeric milliseconds in extra
        let mut conn2 = mock_connection("c2", "codex", "oauth", true, Some("rt_codex_456"), None);
        conn2.data.extra.insert("expiresAt".to_string(), json!(1700000000000_i64));
        let exp2 = extract_token_expiry(&conn2).expect("parsed millis");
        assert_eq!(exp2.timestamp_millis(), 1700000000000);

        // Numeric seconds in extra
        let mut conn3 = mock_connection("c3", "anthropic", "oauth", true, Some("rt_claude_789"), None);
        conn3.data.extra.insert("expires_at".to_string(), json!(1700000000_i64));
        let exp3 = extract_token_expiry(&conn3).expect("parsed seconds");
        assert_eq!(exp3.timestamp(), 1700000000);

        // Missing refresh token
        let conn4 = mock_connection("c4", "openai", "api_key", true, None, None);
        assert_eq!(extract_refresh_token(&conn4), None);
    }

    #[test]
    fn test_identify_expiring_tokens_filtering() {
        let now = Utc::now();
        let buffer_secs = 300; // 5 minutes

        // 1. Expiring in 2 minutes (120s <= 300s): SHOULD BE IDENTIFIED
        let exp_2m = (now + chrono::Duration::seconds(120)).to_rfc3339();
        let conn_due = mock_connection("c_due", "google", "oauth", true, Some("rt_1"), Some(&exp_2m));

        // 2. Already expired 30 seconds ago (-30s <= 300s): SHOULD BE IDENTIFIED
        let exp_expired = (now - chrono::Duration::seconds(30)).to_rfc3339();
        let conn_expired = mock_connection("c_expired", "anthropic", "oauth", true, Some("rt_2"), Some(&exp_expired));

        // 3. Expiring in 10 minutes (600s > 300s): SHOULD NOT BE IDENTIFIED
        let exp_10m = (now + chrono::Duration::seconds(600)).to_rfc3339();
        let conn_not_due = mock_connection("c_fresh", "google", "oauth", true, Some("rt_3"), Some(&exp_10m));

        // 4. Inactive connection: SHOULD NOT BE IDENTIFIED
        let conn_inactive = mock_connection("c_inactive", "google", "oauth", false, Some("rt_4"), Some(&exp_2m));

        // 5. Non-OAuth (API Key) connection: SHOULD NOT BE IDENTIFIED
        let conn_apikey = mock_connection("c_api", "openai", "api_key", true, Some("rt_5"), Some(&exp_2m));

        // 6. OAuth without refresh token: SHOULD NOT BE IDENTIFIED
        let conn_no_rt = mock_connection("c_nort", "google", "oauth", true, None, Some(&exp_2m));

        let list = vec![
            conn_due,
            conn_expired,
            conn_not_due,
            conn_inactive,
            conn_apikey,
            conn_no_rt,
        ];

        let candidates = identify_expiring_tokens(&list, now, buffer_secs);
        assert_eq!(candidates.len(), 2);

        let ids: Vec<&str> = candidates.iter().map(|c| c.connection_id.as_str()).collect();
        assert!(ids.contains(&"c_due"));
        assert!(ids.contains(&"c_expired"));

        let expired_cand = candidates.iter().find(|c| c.connection_id == "c_expired").unwrap();
        assert!(expired_cand.is_expired);

        let due_cand = candidates.iter().find(|c| c.connection_id == "c_due").unwrap();
        assert!(!due_cand.is_expired);
    }

    #[test]
    fn test_pacing_delay_sensitive_vs_normal() {
        let config = TokenRefreshConfig {
            delay_ms: 1500,
            google_delay_ms: 12000,
            ..Default::default()
        };

        // Normal providers
        assert_eq!(get_pacing_delay_ms("openai", &config), 1500);
        assert_eq!(get_pacing_delay_ms("anthropic", &config), 1500);
        assert_eq!(get_pacing_delay_ms("claude", &config), 1500);
        assert_eq!(get_pacing_delay_ms("deepseek", &config), 1500);

        // Sensitive providers
        assert_eq!(get_pacing_delay_ms("google", &config), 12000);
        assert_eq!(get_pacing_delay_ms("gemini", &config), 12000);
        assert_eq!(get_pacing_delay_ms("antigravity", &config), 12000);
        assert_eq!(get_pacing_delay_ms("gemini-cli", &config), 12000);
        assert_eq!(get_pacing_delay_ms("Google", &config), 12000);
    }

    #[tokio::test]
    async fn test_update_connection_tokens_in_database() {
        let db = Database::open_in_memory().unwrap();

        let initial_conn = mock_connection(
            "conn_test_db",
            "google",
            "oauth",
            true,
            Some("rt_initial"),
            Some("2026-11-22T17:00:00Z"),
        );
        db.save_provider_connection(&initial_conn).unwrap();

        let updated = update_connection_tokens(
            &db,
            "conn_test_db",
            "new_access_token_xyz",
            Some("rt_rotated_abc"),
            Some("2026-11-22T18:00:00Z"),
        )
        .unwrap();

        assert_eq!(updated.data.api_key.as_deref(), Some("new_access_token_xyz"));
        assert_eq!(extract_refresh_token(&updated), Some("rt_rotated_abc".to_string()));
        let exp = extract_token_expiry(&updated).expect("parsed updated expiry");
        assert_eq!(exp.to_rfc3339(), "2026-11-22T18:00:00+00:00");

        // Verify loaded from DB
        let reloaded = db.load_provider_connections().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].data.api_key.as_deref(), Some("new_access_token_xyz"));
        assert_eq!(extract_refresh_token(&reloaded[0]), Some("rt_rotated_abc".to_string()));
    }

    #[tokio::test]
    async fn test_model_catalog_sync_skipped_when_disabled() {
        let temp_dir = std::env::temp_dir().join(format!("9r-cat-test-{}", std::process::id()));
        let config = TokenRefreshConfig {
            model_catalog_sync: false,
            ..Default::default()
        };
        let client = reqwest::Client::new();
        let res = sync_model_catalog(&client, &temp_dir, &config, None).await;
        assert_eq!(res.status, "skipped");
        assert_eq!(res.models_count, 0);
    }

    #[tokio::test]
    async fn test_model_catalog_sync_fail_open_on_error() {
        let temp_dir = std::env::temp_dir().join(format!("9r-cat-err-{}", std::process::id()));
        let config = TokenRefreshConfig {
            model_catalog_sync: true,
            catalog_url: "http://127.0.0.1:9".to_string(), // Unbound port
            ..Default::default()
        };
        let client = reqwest::Client::new();
        let res = sync_model_catalog(&client, &temp_dir, &config, None).await;
        assert_eq!(res.status, "error");
        assert!(res.error.is_some());
    }

    #[tokio::test]
    async fn test_worker_disabled_returns_none() {
        let db = Database::open_in_memory().unwrap();
        let temp_dir = std::env::temp_dir();
        let config = TokenRefreshConfig {
            disabled: true,
            ..Default::default()
        };

        let handle = start_background_token_refresh(config, db, &temp_dir);
        assert!(handle.is_none());
    }

    #[tokio::test]
    async fn test_worker_startup_and_clean_shutdown() {
        let db = Database::open_in_memory().unwrap();
        let temp_dir = std::env::temp_dir().join(format!("9r-worker-test-{}", std::process::id()));
        let config = TokenRefreshConfig {
            disabled: false,
            delay_ms: 10,
            google_delay_ms: 20,
            scan_interval_secs: 1,
            model_catalog_sync: false,
            ..Default::default()
        };

        let handle = start_background_token_refresh(config, db, &temp_dir);
        assert!(handle.is_some());

        let h = handle.unwrap();
        // Give worker a moment to enter loop
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Signal clean shutdown
        h.stop();
        h.join_handle.await.unwrap();
    }
}
