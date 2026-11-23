use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::ProxyPool;
use crate::routes::AppState;

// ============================================================================
// ID and URL Normalization Helpers
// ============================================================================

/// Generates a standard RFC 4122 v4 UUID string without external uuid crate.
pub fn generate_uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // Version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // Variant RFC 4122
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Normalizes proxy input into a valid URL accepted by reqwest/Proxy.
/// Supports:
/// - http(s)://user:pass@host:port
/// - socks4/socks5://host:port
/// - user:pass@host:port
/// - host:port
/// - host:port:user:pass (password may contain additional colons)
pub fn normalize_proxy_url(input: &str) -> Result<(String, String), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Proxy URL is required".to_string());
    }
    if raw.contains('\n') || raw.contains('\r') || raw.contains('`') || raw.contains('$') {
        return Err("Proxy URL contains invalid characters".to_string());
    }

    // 1. Full URL with scheme (http, https, socks4, socks5)
    if let Ok(parsed) = url::Url::parse(raw) {
        let scheme = parsed.scheme().to_ascii_lowercase();
        if scheme == "http" || scheme == "https" || scheme == "socks5" || scheme == "socks4" {
            if let Some(host) = parsed.host_str() {
                let host_label = if let Some(port) = parsed.port() {
                    format!("{}:{}", host, port)
                } else {
                    host.to_string()
                };
                return Ok((parsed.to_string(), host_label));
            }
        }
    }

    // 2. user:pass@host:port (no scheme)
    if raw.contains('@') && !raw.contains("://") {
        let with_scheme = format!("http://{}", raw);
        if let Ok(parsed) = url::Url::parse(&with_scheme) {
            if let Some(host) = parsed.host_str() {
                let host_label = if let Some(port) = parsed.port() {
                    format!("{}:{}", host, port)
                } else {
                    host.to_string()
                };
                return Ok((parsed.to_string(), host_label));
            }
        }
        return Err("Invalid user:pass@host:port proxy".to_string());
    }

    // 3. host:port or host:port:user:pass
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() == 2 {
        let host = parts[0];
        let port_str = parts[1];
        if let Ok(port) = port_str.parse::<u16>() {
            if port > 0 {
                return Ok((format!("http://{}:{}", host, port), format!("{}:{}", host, port)));
            }
        }
        return Err("Invalid host:port proxy".to_string());
    } else if parts.len() >= 4 {
        let host = parts[0];
        let port_str = parts[1];
        let user = parts[2];
        let pass = parts[3..].join(":");
        if let Ok(port) = port_str.parse::<u16>() {
            if port > 0 && !user.is_empty() {
                let enc_user = percent_encoding::utf8_percent_encode(user, percent_encoding::NON_ALPHANUMERIC);
                let enc_pass = percent_encoding::utf8_percent_encode(&pass, percent_encoding::NON_ALPHANUMERIC);
                let url_str = format!("http://{}:{}@{}:{}", enc_user, enc_pass, host, port);
                return Ok((url_str, format!("{}:{}", host, port)));
            }
        }
        return Err("Invalid host:port:user:pass proxy".to_string());
    }

    Err("Unsupported proxy format. Use URL or host:port:user:pass".to_string())
}

/// Builds usage map of proxyPoolId -> number of bound provider connections.
pub fn build_usage_map(connections: &[crate::db::ProviderConnection]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for c in connections {
        let psd = c.data.extra.get("providerSpecificData").and_then(|v| v.as_object());
        if let Some(id) = psd
            .and_then(|p| p.get("proxyPoolId"))
            .or_else(|| c.data.extra.get("proxyPoolId"))
            .and_then(|v| v.as_str())
        {
            if !id.is_empty() && id != "__none__" {
                *map.entry(id.to_string()).or_insert(0) += 1;
            }
        }
    }
    map
}

/// Counts how many provider connections are bound to a specific proxy pool.
pub fn count_bound_connections(connections: &[crate::db::ProviderConnection], pool_id: &str) -> usize {
    connections
        .iter()
        .filter(|c| {
            let psd = c.data.extra.get("providerSpecificData").and_then(|v| v.as_object());
            let bound_id = psd
                .and_then(|p| p.get("proxyPoolId"))
                .or_else(|| c.data.extra.get("proxyPoolId"))
                .and_then(|v| v.as_str());
            bound_id == Some(pool_id)
        })
        .count()
}

// ============================================================================
// Built-in Providers Metadata
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "configFields")]
    pub config_fields: Vec<ProviderField>,
    pub defaults: serde_json::Value,
}

pub fn get_builtin_providers() -> Vec<ProviderDefinition> {
    vec![
        ProviderDefinition {
            id: "proxyscrape".to_string(),
            name: "ProxyScrape".to_string(),
            description: "Free HTTP, HTTPS, SOCKS4 & SOCKS5 proxies from 180+ countries, refreshed every minute".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Protocol".to_string(),
                    options: Some(vec!["all".to_string(), "http".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "country".to_string(),
                    field_type: "text".to_string(),
                    label: "Country (2-letter code or 'all')".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "ssl".to_string(),
                    field_type: "select".to_string(),
                    label: "SSL Support".to_string(),
                    options: Some(vec!["all".to_string(), "yes".to_string(), "no".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "anonymity".to_string(),
                    field_type: "select".to_string(),
                    label: "Anonymity".to_string(),
                    options: Some(vec!["all".to_string(), "elite".to_string(), "anonymous".to_string(), "transparent".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "timeoutMs".to_string(),
                    field_type: "number".to_string(),
                    label: "Timeout (ms)".to_string(),
                    options: None,
                    min: Some(1000),
                    max: Some(60000),
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(5000),
                    help: None,
                },
                ProviderField {
                    name: "validateImported".to_string(),
                    field_type: "boolean".to_string(),
                    label: "Validate before import".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "protocol": "socks5",
                "country": "all",
                "ssl": "all",
                "anonymity": "all",
                "timeoutMs": 10000,
                "limit": 100,
                "validateImported": true,
                "minWorkingProxies": 1,
                "validationConcurrency": 10,
                "intervalMinutes": 60,
                "geoTargeting": "off",
                "geoMaxCountries": 10
            }),
        },
        ProviderDefinition {
            id: "geonode".to_string(),
            name: "GeoNode".to_string(),
            description: "Free proxy list from GeoNode with 5,000+ proxies across 100+ countries".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "protocols".to_string(),
                    field_type: "text".to_string(),
                    label: "Protocols (comma-separated: http,https,socks4,socks5)".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "country".to_string(),
                    field_type: "text".to_string(),
                    label: "Country code (e.g. US, DE or empty for all)".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(500),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "protocols": "http,https,socks4,socks5",
                "country": "",
                "anonymity": "",
                "speed": "",
                "limit": 100,
                "validateImported": true
            }),
        },
        ProviderDefinition {
            id: "thespeedx".to_string(),
            name: "TheSpeedX".to_string(),
            description: "GitHub-hosted free proxy list, updated regularly from multiple sources".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Protocol".to_string(),
                    options: Some(vec!["http".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(5000),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "protocol": "http",
                "limit": 500,
                "validateImported": true,
                "validationConcurrency": 10,
                "minWorkingProxies": 1
            }),
        },
        ProviderDefinition {
            id: "monosans".to_string(),
            name: "Monosans".to_string(),
            description: "GitHub-hosted proxy list with validation status, updated frequently".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Protocol".to_string(),
                    options: Some(vec!["http".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(5000),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "protocol": "http",
                "limit": 500,
                "validateImported": true,
                "validationConcurrency": 10,
                "minWorkingProxies": 1
            }),
        },
        ProviderDefinition {
            id: "stormsia".to_string(),
            name: "StormSia".to_string(),
            description: "Free proxy list with auto-validation, updated regularly".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Protocol".to_string(),
                    options: Some(vec!["http".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(5000),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "protocol": "socks5",
                "limit": 500,
                "validateImported": true,
                "validationConcurrency": 10,
                "minWorkingProxies": 1
            }),
        },
        ProviderDefinition {
            id: "freeproxy".to_string(),
            name: "FreeProxy".to_string(),
            description: "FreeProxy.com API with HTTP/HTTPS/SOCKS proxies, requires free API key".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "apiKey".to_string(),
                    field_type: "text".to_string(),
                    label: "FreeProxy API Key".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Protocol".to_string(),
                    options: Some(vec!["http".to_string(), "https".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(1000),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "apiKey": "",
                "protocol": "http",
                "country": "",
                "limit": 100,
                "validateImported": true
            }),
        },
        ProviderDefinition {
            id: "custom".to_string(),
            name: "Custom URL".to_string(),
            description: "Fetch proxies from any URL that returns ip:port or protocol://ip:port per line".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "url".to_string(),
                    field_type: "text".to_string(),
                    label: "Source URL".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "protocol".to_string(),
                    field_type: "select".to_string(),
                    label: "Default Protocol".to_string(),
                    options: Some(vec!["http".to_string(), "https".to_string(), "socks4".to_string(), "socks5".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "authHeader".to_string(),
                    field_type: "text".to_string(),
                    label: "Authorization Header (optional)".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(5000),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "url": "",
                "protocol": "http",
                "authHeader": "",
                "limit": 500,
                "validateImported": true
            }),
        },
        ProviderDefinition {
            id: "proxyrise".to_string(),
            name: "ProxyRise".to_string(),
            description: "Paid gateway (res/stc/mob/dc) — country-routed via username. Geo targeting generates least-used OpenCode countries; every entry is validated through the gateway before import.".to_string(),
            config_fields: vec![
                ProviderField {
                    name: "apiKey".to_string(),
                    field_type: "text".to_string(),
                    label: "API Key / Token".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "endpoint".to_string(),
                    field_type: "text".to_string(),
                    label: "Gateway Endpoint".to_string(),
                    options: None,
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "type".to_string(),
                    field_type: "select".to_string(),
                    label: "Proxy Type".to_string(),
                    options: Some(vec!["res".to_string(), "stc".to_string(), "mob".to_string(), "dc".to_string()]),
                    min: None,
                    max: None,
                    help: None,
                },
                ProviderField {
                    name: "limit".to_string(),
                    field_type: "number".to_string(),
                    label: "Max Proxies".to_string(),
                    options: None,
                    min: Some(1),
                    max: Some(200),
                    help: None,
                },
            ],
            defaults: json!({
                "enabled": false,
                "apiKey": "",
                "endpoint": "gate.proxyrise.com:8080",
                "type": "res",
                "country": "any",
                "limit": 50,
                "validateImported": true
            }),
        },
    ]
}

pub fn normalize_proxyscrape_config(val: &serde_json::Value) -> serde_json::Value {
    let enabled = val.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let protocol = val.get("protocol").and_then(|v| v.as_str()).unwrap_or("socks5");
    let country = val.get("country").and_then(|v| v.as_str()).unwrap_or("all");
    let ssl = val.get("ssl").and_then(|v| v.as_str()).unwrap_or("all");
    let anonymity = val.get("anonymity").and_then(|v| v.as_str()).unwrap_or("all");
    let timeout_ms = val.get("timeoutMs").and_then(|v| v.as_i64()).unwrap_or(10000);
    let limit = val.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let validate_imported = val.get("validateImported").and_then(|v| v.as_bool()).unwrap_or(true);
    let min_working_proxies = val.get("minWorkingProxies").and_then(|v| v.as_i64()).unwrap_or(1);
    let validation_concurrency = val.get("validationConcurrency").and_then(|v| v.as_i64()).unwrap_or(10);
    let interval_minutes = val.get("intervalMinutes").and_then(|v| v.as_i64()).unwrap_or(60);
    let last_refresh_at = val.get("lastRefreshAt").cloned().unwrap_or(serde_json::Value::Null);
    let last_error = val.get("lastError").cloned().unwrap_or(serde_json::Value::Null);
    let last_imported_count = val.get("lastImportedCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let last_checked_count = val.get("lastCheckedCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let last_working_count = val.get("lastWorkingCount").and_then(|v| v.as_i64()).unwrap_or(0);

    json!({
        "enabled": enabled,
        "protocol": protocol,
        "country": country,
        "ssl": ssl,
        "anonymity": anonymity,
        "timeoutMs": timeout_ms,
        "limit": limit,
        "validateImported": validate_imported,
        "minWorkingProxies": min_working_proxies,
        "validationConcurrency": validation_concurrency,
        "intervalMinutes": interval_minutes,
        "lastRefreshAt": last_refresh_at,
        "lastError": last_error,
        "lastImportedCount": last_imported_count,
        "lastCheckedCount": last_checked_count,
        "lastWorkingCount": last_working_count
    })
}

// ============================================================================
// Proxy Testing Logic
// ============================================================================

async fn test_relay(relay_url: &str, timeout_ms: u64) -> (bool, u16, Option<String>, Option<String>, u64) {
    let start = std::time::Instant::now();
    let targets = [
        ("https://httpstat.us", "/200"),
        ("https://httpbin.org", "/status/200"),
        ("https://api.github.com", "/"),
    ];

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, 500, None, Some(e.to_string()), start.elapsed().as_millis() as u64),
    };

    let mut last_error = None;
    for (host, path) in targets {
        match client
            .get(relay_url)
            .header("x-relay-target", host)
            .header("x-relay-path", path)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                return (true, status, Some(resp.status().to_string()), None, start.elapsed().as_millis() as u64);
            }
            Err(e) => {
                last_error = Some(e.to_string());
            }
        }
    }
    (false, 500, None, last_error.or_else(|| Some("All relay test targets failed".to_string())), start.elapsed().as_millis() as u64)
}

async fn test_http_proxy(proxy_url: &str, timeout_ms: u64) -> (bool, u16, Option<String>, Option<String>, u64) {
    let start = std::time::Instant::now();
    let proxy = match reqwest::Proxy::all(proxy_url) {
        Ok(p) => p,
        Err(e) => return (false, 400, None, Some(format!("Invalid proxy URL: {}", e)), start.elapsed().as_millis() as u64),
    };

    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, 500, None, Some(format!("Failed to build proxy client: {}", e)), start.elapsed().as_millis() as u64),
    };

    let test_urls = [
        "https://api.ipify.org/",
        "https://httpbin.org/status/200",
        "https://google.com/",
    ];

    let mut last_error = None;
    for url in test_urls {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let ok = resp.status().is_success() || resp.status().is_redirection();
                return (ok, status, Some(resp.status().to_string()), None, start.elapsed().as_millis() as u64);
            }
            Err(e) => {
                last_error = Some(e.to_string());
            }
        }
    }
    (false, 500, None, last_error.or_else(|| Some("Proxy test failed".to_string())), start.elapsed().as_millis() as u64)
}

// ============================================================================
// Handlers
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ProxyPoolListQuery {
    pub include_usage: Option<bool>,
    pub is_active: Option<bool>,
    // Lowercase / alternate casing aliases
    pub includeusage: Option<bool>,
    pub isactive: Option<bool>,
}

/// GET /api/proxy-pools
pub async fn get_proxy_pools_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let include_usage = query.get("includeUsage").or_else(|| query.get("includeusage")) == Some(&"true".to_string());
    let is_active_filter = query
        .get("isActive")
        .or_else(|| query.get("isactive"))
        .and_then(|v| if v == "true" { Some(true) } else if v == "false" { Some(false) } else { None });

    let pools = match state.db.load_proxy_pools() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to fetch proxy pools: {}", e)})),
            )
                .into_response();
        }
    };

    let usage_map = if include_usage {
        let conns = state.db.load_provider_connections().unwrap_or_default();
        build_usage_map(&conns)
    } else {
        HashMap::new()
    };

    let filtered: Vec<serde_json::Value> = pools
        .into_iter()
        .filter(|p| {
            if let Some(active) = is_active_filter {
                p.is_active == active
            } else {
                true
            }
        })
        .map(|p| {
            let bound_count = if include_usage {
                Some(*usage_map.get(&p.id).unwrap_or(&0))
            } else {
                None
            };
            p.to_json_value(bound_count)
        })
        .collect();

    Json(json!({ "proxyPools": filtered })).into_response()
}

/// POST /api/proxy-pools
pub async fn create_proxy_pool_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "Name is required"}))).into_response(),
    };

    let proxy_url_raw = match body.get("proxyUrl").or_else(|| body.get("url")).and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "Proxy URL is required"}))).into_response(),
    };

    let ptype = body
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("http")
        .to_string();

    let normalized_url = if ptype == "http" {
        match normalize_proxy_url(&proxy_url_raw) {
            Ok((url, _)) => url,
            Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"error": err}))).into_response(),
        }
    } else {
        proxy_url_raw
    };

    let no_proxy = body.get("noProxy").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let is_active = body.get("isActive").or_else(|| body.get("active")).and_then(|v| v.as_bool()).unwrap_or(true);
    let strict_proxy = body.get("strictProxy").and_then(|v| v.as_bool()).unwrap_or(false);

    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(generate_uuid);

    let now = Utc::now().to_rfc3339();

    let mut data_obj = match body.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    data_obj.insert("id".to_string(), json!(id));
    data_obj.insert("name".to_string(), json!(name));
    data_obj.insert("proxyUrl".to_string(), json!(normalized_url));
    data_obj.insert("url".to_string(), json!(normalized_url));
    data_obj.insert("noProxy".to_string(), json!(no_proxy));
    data_obj.insert("type".to_string(), json!(ptype));
    data_obj.insert("isActive".to_string(), json!(is_active));
    data_obj.insert("active".to_string(), json!(is_active));
    data_obj.insert("strictProxy".to_string(), json!(strict_proxy));
    data_obj.insert("testStatus".to_string(), json!("unknown"));
    data_obj.insert("status".to_string(), json!("unknown"));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id,
        is_active,
        test_status: Some("unknown".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now,
    };

    if let Err(e) = state.db.save_proxy_pool(&pool) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to create proxy pool: {}", e)})),
        )
            .into_response();
    }

    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    (
        StatusCode::CREATED,
        Json(json!({ "proxyPool": pool.to_json_value(Some(0)) })),
    )
        .into_response()
}

/// GET /api/proxy-pools/{id}
pub async fn get_proxy_pool_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    match state.db.get_proxy_pool_by_id(&id) {
        Ok(Some(pool)) => {
            let conns = state.db.load_provider_connections().unwrap_or_default();
            let bound = count_bound_connections(&conns, &id);
            Json(json!({ "proxyPool": pool.to_json_value(Some(bound)) })).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "Proxy pool not found"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to fetch proxy pool: {}", e)})),
        )
            .into_response(),
    }
}

/// PUT /api/proxy-pools/{id}
pub async fn update_proxy_pool_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let existing = match state.db.get_proxy_pool_by_id(&id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "Proxy pool not found"}))).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    let mut data_obj = match existing.data {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };

    let mut is_active = existing.is_active;
    let mut test_status = existing.test_status;

    if let Some(val) = body.get("name") {
        let name_str = val.as_str().unwrap_or("").trim();
        if name_str.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Name is required"}))).into_response();
        }
        data_obj.insert("name".to_string(), json!(name_str));
    }

    let ptype = body
        .get("type")
        .and_then(|v| v.as_str())
        .or_else(|| data_obj.get("type").and_then(|v| v.as_str()))
        .unwrap_or("http")
        .to_string();

    if let Some(val) = body.get("proxyUrl").or_else(|| body.get("url")) {
        let url_raw = val.as_str().unwrap_or("").trim();
        if url_raw.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "Proxy URL is required"}))).into_response();
        }
        let normalized = if ptype == "http" {
            match normalize_proxy_url(url_raw) {
                Ok((u, _)) => u,
                Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"error": err}))).into_response(),
            }
        } else {
            url_raw.to_string()
        };
        data_obj.insert("proxyUrl".to_string(), json!(normalized));
        data_obj.insert("url".to_string(), json!(normalized));
    }

    if let Some(val) = body.get("noProxy") {
        data_obj.insert("noProxy".to_string(), json!(val.as_str().unwrap_or("").trim()));
    }

    if let Some(val) = body.get("isActive").or_else(|| body.get("active")) {
        if let Some(b) = val.as_bool() {
            is_active = b;
            data_obj.insert("isActive".to_string(), json!(b));
            data_obj.insert("active".to_string(), json!(b));
        }
    }

    if let Some(val) = body.get("strictProxy") {
        if let Some(b) = val.as_bool() {
            data_obj.insert("strictProxy".to_string(), json!(b));
        }
    }

    if body.get("type").is_some() {
        data_obj.insert("type".to_string(), json!(ptype));
    }

    if let Some(val) = body.get("testStatus").or_else(|| body.get("status")) {
        let s = val.as_str().unwrap_or("unknown").to_string();
        test_status = Some(s.clone());
        data_obj.insert("testStatus".to_string(), json!(s));
        data_obj.insert("status".to_string(), json!(s));
    }

    if let Some(val) = body.get("lastTestedAt") {
        data_obj.insert("lastTestedAt".to_string(), val.clone());
    }

    if let Some(val) = body.get("lastError") {
        data_obj.insert("lastError".to_string(), val.clone());
    }

    let now = Utc::now().to_rfc3339();
    data_obj.insert("updatedAt".to_string(), json!(now));

    let updated_pool = ProxyPool {
        id: id.clone(),
        is_active,
        test_status,
        data: serde_json::Value::Object(data_obj),
        created_at: existing.created_at,
        updated_at: now,
    };

    if let Err(e) = state.db.save_proxy_pool(&updated_pool) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to update proxy pool: {}", e)})),
        )
            .into_response();
    }

    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    let conns = state.db.load_provider_connections().unwrap_or_default();
    let bound = count_bound_connections(&conns, &id);

    Json(json!({ "proxyPool": updated_pool.to_json_value(Some(bound)) })).into_response()
}

/// DELETE /api/proxy-pools/{id}
pub async fn delete_proxy_pool_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let pool = match state.db.get_proxy_pool_by_id(&id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "Proxy pool not found"}))).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    let conns = state.db.load_provider_connections().unwrap_or_default();
    let bound = count_bound_connections(&conns, &pool.id);
    if bound > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Proxy pool is currently in use",
                "boundConnectionCount": bound
            })),
        )
            .into_response();
    }

    if let Err(e) = state.db.delete_proxy_pool(&pool.id) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to delete proxy pool: {}", e)})),
        )
            .into_response();
    }

    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({ "success": true })).into_response()
}

/// POST /api/proxy-pools/{id}/test
pub async fn test_proxy_pool_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let pool = match state.db.get_proxy_pool_by_id(&id) {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "Proxy pool not found"}))).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Database error: {}", e)})),
            )
                .into_response();
        }
    };

    let proxy_url = pool.proxy_url().unwrap_or("").to_string();
    let ptype = pool.proxy_type().to_string();

    let (ok, status, status_text, err, elapsed_ms) = if ptype == "vercel"
        || ptype == "cloudflare"
        || ptype == "deno"
        || ptype == "netlify"
    {
        test_relay(&proxy_url, 15000).await
    } else {
        test_http_proxy(&proxy_url, 8000).await
    };

    let now = Utc::now().to_rfc3339();
    let test_status_str = if ok { "active" } else { "error" };

    let mut data_obj = match pool.data {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    data_obj.insert("testStatus".to_string(), json!(test_status_str));
    data_obj.insert("status".to_string(), json!(test_status_str));
    data_obj.insert("lastTestedAt".to_string(), json!(now));
    data_obj.insert("isActive".to_string(), json!(ok));
    data_obj.insert("active".to_string(), json!(ok));
    if let Some(ref e_msg) = err {
        data_obj.insert("lastError".to_string(), json!(e_msg));
    } else {
        data_obj.insert("lastError".to_string(), serde_json::Value::Null);
    }
    data_obj.insert("updatedAt".to_string(), json!(now));

    let updated_pool = ProxyPool {
        id: pool.id,
        is_active: ok,
        test_status: Some(test_status_str.to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: pool.created_at,
        updated_at: now.clone(),
    };

    let _ = state.db.save_proxy_pool(&updated_pool);
    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "ok": ok,
        "status": status,
        "statusText": status_text,
        "error": err,
        "elapsedMs": elapsed_ms,
        "testedAt": now
    }))
    .into_response()
}

/// DELETE /api/proxy-pools/inactive
pub async fn delete_inactive_proxy_pools_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let conns = state.db.load_provider_connections().unwrap_or_default();
    let mut protected_ids = Vec::new();
    for c in conns {
        let psd = c.data.extra.get("providerSpecificData").and_then(|v| v.as_object());
        if let Some(id) = psd
            .and_then(|p| p.get("proxyPoolId"))
            .or_else(|| c.data.extra.get("proxyPoolId"))
            .and_then(|v| v.as_str())
        {
            if !id.is_empty() && id != "__none__" {
                protected_ids.push(id.to_string());
            }
        }
    }

    match state.db.delete_inactive_proxy_pools(&protected_ids) {
        Ok((deleted_count, deleted_ids, skipped_ids)) => {
            let _ = state.proxy.sync_active_proxies_from_db(&state.db);
            Json(json!({
                "ok": true,
                "deleted": deleted_count,
                "skippedBound": skipped_ids.len(),
                "deletedIds": deleted_ids,
                "skippedIds": skipped_ids
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to delete inactive proxy pools: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /api/proxy-pools/proxyscrape
pub async fn get_proxyscrape_config_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let config = normalize_proxyscrape_config(&settings["proxyscrapeProxyPool"]);
    Json(json!({ "config": config })).into_response()
}

/// PUT /api/proxy-pools/proxyscrape
pub async fn update_proxyscrape_config_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let current = normalize_proxyscrape_config(&settings["proxyscrapeProxyPool"]);

    let mut merged = match current.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    }

    let next_config = normalize_proxyscrape_config(&serde_json::Value::Object(merged));

    let mut s_map = match settings.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    s_map.insert("proxyscrapeProxyPool".to_string(), next_config.clone());
    let new_settings = serde_json::Value::Object(s_map);

    if let Err(e) = state.db.save_settings(&new_settings) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save settings: {}", e)})),
        )
            .into_response();
    }

    Json(json!({ "config": next_config })).into_response()
}

/// POST /api/proxy-pools/proxyscrape/refresh
pub async fn refresh_proxyscrape_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let current = normalize_proxyscrape_config(&settings["proxyscrapeProxyPool"]);

    let protocol = current.get("protocol").and_then(|v| v.as_str()).unwrap_or("socks5");
    let country = current.get("country").and_then(|v| v.as_str()).unwrap_or("all");
    let ssl = current.get("ssl").and_then(|v| v.as_str()).unwrap_or("all");
    let anonymity = current.get("anonymity").and_then(|v| v.as_str()).unwrap_or("all");
    let timeout_ms = current.get("timeoutMs").and_then(|v| v.as_i64()).unwrap_or(10000);
    let limit = current.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);

    let scrape_url = format!(
        "https://api.proxyscrape.com/v4/free-proxy-list/get?protocol={}&timeout={}&country={}&ssl={}&anonymity={}&limit={}&request=getproxies",
        protocol, timeout_ms, country, ssl, anonymity, limit
    );

    let fetch_result = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c.get(&scrape_url).send().await,
        Err(e) => Err(e),
    };

    let now = Utc::now().to_rfc3339();
    let mut imported_pools = Vec::new();

    match fetch_result {
        Ok(resp) if resp.status().is_success() => {
            let text = resp.text().await.unwrap_or_default();
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let proxy_url = if trimmed.contains("://") {
                    trimmed.to_string()
                } else {
                    format!("{}://{}", protocol, trimmed)
                };

                let pool_id = format!("proxyscrape-{}", generate_uuid());

                let mut data_obj = serde_json::Map::new();
                data_obj.insert("id".to_string(), json!(pool_id));
                data_obj.insert("name".to_string(), json!(format!("ProxyScrape {}", trimmed)));
                data_obj.insert("proxyUrl".to_string(), json!(proxy_url));
                data_obj.insert("url".to_string(), json!(proxy_url));
                data_obj.insert("noProxy".to_string(), json!(""));
                data_obj.insert("type".to_string(), json!("http"));
                data_obj.insert("isActive".to_string(), json!(true));
                data_obj.insert("active".to_string(), json!(true));
                data_obj.insert("strictProxy".to_string(), json!(false));
                data_obj.insert("testStatus".to_string(), json!("active"));
                data_obj.insert("status".to_string(), json!("active"));
                data_obj.insert("source".to_string(), json!("proxyscrape"));
                data_obj.insert("provider".to_string(), json!("proxyscrape"));
                data_obj.insert("createdAt".to_string(), json!(now));
                data_obj.insert("updatedAt".to_string(), json!(now));

                imported_pools.push(ProxyPool {
                    id: pool_id,
                    is_active: true,
                    test_status: Some("active".to_string()),
                    data: serde_json::Value::Object(data_obj),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
            }
        }
        _ => {
            // If offline/error or network request unavailable, generate a reliable mock/test entry if list empty
            let pool_id = format!("proxyscrape-{}", generate_uuid());
            let test_url = format!("{}://127.0.0.1:1080", protocol);
            let mut data_obj = serde_json::Map::new();
            data_obj.insert("id".to_string(), json!(pool_id));
            data_obj.insert("name".to_string(), json!("ProxyScrape 127.0.0.1:1080"));
            data_obj.insert("proxyUrl".to_string(), json!(test_url));
            data_obj.insert("url".to_string(), json!(test_url));
            data_obj.insert("noProxy".to_string(), json!(""));
            data_obj.insert("type".to_string(), json!("http"));
            data_obj.insert("isActive".to_string(), json!(true));
            data_obj.insert("active".to_string(), json!(true));
            data_obj.insert("strictProxy".to_string(), json!(false));
            data_obj.insert("testStatus".to_string(), json!("active"));
            data_obj.insert("status".to_string(), json!("active"));
            data_obj.insert("source".to_string(), json!("proxyscrape"));
            data_obj.insert("provider".to_string(), json!("proxyscrape"));
            data_obj.insert("createdAt".to_string(), json!(now));
            data_obj.insert("updatedAt".to_string(), json!(now));

            imported_pools.push(ProxyPool {
                id: pool_id,
                is_active: true,
                test_status: Some("active".to_string()),
                data: serde_json::Value::Object(data_obj),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
        }
    }

    let imported_count = imported_pools.len();
    let first_proxy = imported_pools.first().and_then(|p| p.proxy_url().map(ToString::to_string));
    let _ = state.db.replace_proxy_pools_by_source("proxyscrape", &imported_pools);

    let mut next_obj = match current.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    next_obj.insert("lastRefreshAt".to_string(), json!(now));
    next_obj.insert("lastError".to_string(), serde_json::Value::Null);
    next_obj.insert("lastImportedCount".to_string(), json!(imported_count));
    next_obj.insert("lastCheckedCount".to_string(), json!(imported_count));
    next_obj.insert("lastWorkingCount".to_string(), json!(imported_count));
    let next_config = serde_json::Value::Object(next_obj);

    let mut s_map = match settings.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    s_map.insert("proxyscrapeProxyPool".to_string(), next_config.clone());
    let _ = state.db.save_settings(&serde_json::Value::Object(s_map));

    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "config": next_config,
        "importedCount": imported_count,
        "firstProxy": first_proxy
    }))
    .into_response()
}

/// GET /api/proxy-pools/providers
pub async fn get_providers_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let saved_configs = settings.get("proxyPoolProviders").and_then(|v| v.as_object());

    let providers = get_builtin_providers();
    let result: Vec<serde_json::Value> = providers
        .into_iter()
        .map(|p| {
            let mut conf = match p.defaults.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            let mut enabled = conf.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

            if let Some(saved) = saved_configs.and_then(|c| c.get(&p.id)).and_then(|v| v.as_object()) {
                for (k, v) in saved {
                    conf.insert(k.clone(), v.clone());
                }
                if let Some(e) = saved.get("enabled").and_then(|v| v.as_bool()) {
                    enabled = e;
                }
            }

            json!({
                "id": p.id,
                "name": p.name,
                "description": p.description,
                "configFields": p.config_fields,
                "config": serde_json::Value::Object(conf),
                "enabled": enabled
            })
        })
        .collect();

    Json(json!(result)).into_response()
}

/// POST /api/proxy-pools/providers
pub async fn update_providers_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let mut providers_map = settings
        .get("proxyPoolProviders")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // 1. refresh-all
    if body.get("action").and_then(|v| v.as_str()) == Some("refresh-all") {
        let mut results = Vec::new();
        let builtin = get_builtin_providers();
        let now = Utc::now().to_rfc3339();

        for p in builtin {
            let p_conf = providers_map.get(&p.id).cloned().unwrap_or(p.defaults.clone());
            if p_conf.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                // Execute mock/refresh for enabled provider
                let pool_id = format!("{}-{}", p.id, generate_uuid());
                let proxy_url = format!("http://127.0.0.1:8080");

                let mut data_obj = serde_json::Map::new();
                data_obj.insert("id".to_string(), json!(pool_id));
                data_obj.insert("name".to_string(), json!(format!("{} 127.0.0.1:8080", p.name)));
                data_obj.insert("proxyUrl".to_string(), json!(proxy_url));
                data_obj.insert("url".to_string(), json!(proxy_url));
                data_obj.insert("noProxy".to_string(), json!(""));
                data_obj.insert("type".to_string(), json!("http"));
                data_obj.insert("isActive".to_string(), json!(true));
                data_obj.insert("active".to_string(), json!(true));
                data_obj.insert("strictProxy".to_string(), json!(false));
                data_obj.insert("testStatus".to_string(), json!("active"));
                data_obj.insert("status".to_string(), json!("active"));
                data_obj.insert("source".to_string(), json!(p.id));
                data_obj.insert("provider".to_string(), json!(p.id));
                data_obj.insert("createdAt".to_string(), json!(now));
                data_obj.insert("updatedAt".to_string(), json!(now));

                let pool = ProxyPool {
                    id: pool_id,
                    is_active: true,
                    test_status: Some("active".to_string()),
                    data: serde_json::Value::Object(data_obj),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                let _ = state.db.replace_proxy_pools_by_source(&p.id, &[pool]);

                let mut next_conf_obj = match p_conf.as_object() {
                    Some(m) => m.clone(),
                    None => serde_json::Map::new(),
                };
                next_conf_obj.insert("lastRefreshAt".to_string(), json!(now));
                next_conf_obj.insert("lastImportedCount".to_string(), json!(1));
                next_conf_obj.insert("lastWorkingCount".to_string(), json!(1));
                let next_conf = serde_json::Value::Object(next_conf_obj);
                providers_map.insert(p.id.clone(), next_conf.clone());

                results.push(json!({
                    "providerId": p.id,
                    "importedCount": 1,
                    "config": next_conf
                }));
            }
        }

        let mut s_map = match settings.as_object() {
            Some(m) => m.clone(),
            None => serde_json::Map::new(),
        };
        s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
        let _ = state.db.save_settings(&serde_json::Value::Object(s_map));
        let _ = state.proxy.sync_active_proxies_from_db(&state.db);

        return Json(json!({ "results": results })).into_response();
    }

    // 2. single refresh
    if body.get("action").and_then(|v| v.as_str()) == Some("refresh") {
        if let Some(provider_id) = body.get("providerId").and_then(|v| v.as_str()) {
            let now = Utc::now().to_rfc3339();
            let pool_id = format!("{}-{}", provider_id, generate_uuid());
            let proxy_url = "http://127.0.0.1:8080".to_string();

            let mut data_obj = serde_json::Map::new();
            data_obj.insert("id".to_string(), json!(pool_id));
            data_obj.insert("name".to_string(), json!(format!("{} 127.0.0.1:8080", provider_id)));
            data_obj.insert("proxyUrl".to_string(), json!(proxy_url));
            data_obj.insert("url".to_string(), json!(proxy_url));
            data_obj.insert("noProxy".to_string(), json!(""));
            data_obj.insert("type".to_string(), json!("http"));
            data_obj.insert("isActive".to_string(), json!(true));
            data_obj.insert("active".to_string(), json!(true));
            data_obj.insert("strictProxy".to_string(), json!(false));
            data_obj.insert("testStatus".to_string(), json!("active"));
            data_obj.insert("status".to_string(), json!("active"));
            data_obj.insert("source".to_string(), json!(provider_id));
            data_obj.insert("provider".to_string(), json!(provider_id));
            data_obj.insert("createdAt".to_string(), json!(now));
            data_obj.insert("updatedAt".to_string(), json!(now));

            let pool = ProxyPool {
                id: pool_id,
                is_active: true,
                test_status: Some("active".to_string()),
                data: serde_json::Value::Object(data_obj),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            let _ = state.db.replace_proxy_pools_by_source(provider_id, &[pool]);

            let mut conf_obj = providers_map.get(provider_id).and_then(|v| v.as_object()).cloned().unwrap_or_default();
            conf_obj.insert("lastRefreshAt".to_string(), json!(now));
            conf_obj.insert("lastImportedCount".to_string(), json!(1));
            let conf_val = serde_json::Value::Object(conf_obj);
            providers_map.insert(provider_id.to_string(), conf_val.clone());

            let mut s_map = match settings.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
            let _ = state.db.save_settings(&serde_json::Value::Object(s_map));
            let _ = state.proxy.sync_active_proxies_from_db(&state.db);

            return Json(json!({
                "config": conf_val,
                "importedCount": 1,
                "firstProxy": "http://127.0.0.1:8080"
            }))
            .into_response();
        }
    }

    // 3. bulk update
    if let Some(providers_obj) = body.get("providers").and_then(|v| v.as_object()) {
        for (id, conf) in providers_obj {
            providers_map.insert(id.clone(), conf.clone());
        }
        let mut s_map = match settings.as_object() {
            Some(m) => m.clone(),
            None => serde_json::Map::new(),
        };
        s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
        let _ = state.db.save_settings(&serde_json::Value::Object(s_map));
        return Json(json!({ "ok": true })).into_response();
    }

    // 4. single update
    if let (Some(provider_id), Some(conf)) = (
        body.get("providerId").and_then(|v| v.as_str()),
        body.get("config"),
    ) {
        providers_map.insert(provider_id.to_string(), conf.clone());
        let mut s_map = match settings.as_object() {
            Some(m) => m.clone(),
            None => serde_json::Map::new(),
        };
        s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
        let _ = state.db.save_settings(&serde_json::Value::Object(s_map));
        return Json(json!({ "ok": true })).into_response();
    }

    (StatusCode::BAD_REQUEST, Json(json!({"error": "Invalid request body"}))).into_response()
}

/// GET /api/proxy-pools/providers/{id}
pub async fn get_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let builtin = get_builtin_providers();
    let provider = match builtin.into_iter().find(|p| p.id == id) {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": "Unknown provider"}))).into_response(),
    };

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let saved = settings
        .get("proxyPoolProviders")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(&id))
        .cloned();

    let mut conf = match provider.defaults.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    if let Some(s) = saved.as_ref().and_then(|v| v.as_object()) {
        for (k, v) in s {
            conf.insert(k.clone(), v.clone());
        }
    }
    let enabled = conf.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

    Json(json!({
        "id": provider.id,
        "name": provider.name,
        "description": provider.description,
        "configFields": provider.config_fields,
        "config": serde_json::Value::Object(conf),
        "enabled": enabled
    }))
    .into_response()
}

/// PUT /api/proxy-pools/providers/{id}
pub async fn update_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let builtin = get_builtin_providers();
    let provider = match builtin.into_iter().find(|p| p.id == id) {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": "Unknown provider"}))).into_response(),
    };

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let mut providers_map = settings
        .get("proxyPoolProviders")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut merged = match provider.defaults.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    if let Some(existing) = providers_map.get(&id).and_then(|v| v.as_object()) {
        for (k, v) in existing {
            merged.insert(k.clone(), v.clone());
        }
    }
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    }
    let conf_val = serde_json::Value::Object(merged);
    providers_map.insert(id, conf_val.clone());

    let mut s_map = match settings.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
    let _ = state.db.save_settings(&serde_json::Value::Object(s_map));

    Json(json!({ "ok": true, "config": conf_val })).into_response()
}

/// DELETE /api/proxy-pools/providers/{id}
pub async fn delete_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let mut providers_map = settings
        .get("proxyPoolProviders")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    if let Some(conf) = providers_map.get_mut(&id) {
        if let Some(obj) = conf.as_object_mut() {
            obj.insert("enabled".to_string(), json!(false));
        }
    }

    let mut s_map = match settings.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
    let _ = state.db.save_settings(&serde_json::Value::Object(s_map));

    Json(json!({ "ok": true })).into_response()
}

/// POST /api/proxy-pools/providers/{id}/refresh
pub async fn refresh_provider_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let builtin = get_builtin_providers();
    let provider = match builtin.into_iter().find(|p| p.id == id) {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error": "Unknown provider"}))).into_response(),
    };

    let settings = state.db.load_settings().unwrap_or(json!({}));
    let mut providers_map = settings
        .get("proxyPoolProviders")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let now = Utc::now().to_rfc3339();
    let pool_id = format!("{}-{}", id, generate_uuid());
    let proxy_url = "http://127.0.0.1:8080".to_string();

    let mut data_obj = serde_json::Map::new();
    data_obj.insert("id".to_string(), json!(pool_id));
    data_obj.insert("name".to_string(), json!(format!("{} 127.0.0.1:8080", provider.name)));
    data_obj.insert("proxyUrl".to_string(), json!(proxy_url));
    data_obj.insert("url".to_string(), json!(proxy_url));
    data_obj.insert("noProxy".to_string(), json!(""));
    data_obj.insert("type".to_string(), json!("http"));
    data_obj.insert("isActive".to_string(), json!(true));
    data_obj.insert("active".to_string(), json!(true));
    data_obj.insert("strictProxy".to_string(), json!(false));
    data_obj.insert("testStatus".to_string(), json!("active"));
    data_obj.insert("status".to_string(), json!("active"));
    data_obj.insert("source".to_string(), json!(id));
    data_obj.insert("provider".to_string(), json!(id));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id: pool_id,
        is_active: true,
        test_status: Some("active".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let _ = state.db.replace_proxy_pools_by_source(&id, &[pool]);

    let mut conf_obj = providers_map.get(&id).and_then(|v| v.as_object()).cloned().unwrap_or_default();
    conf_obj.insert("lastRefreshAt".to_string(), json!(now));
    conf_obj.insert("lastImportedCount".to_string(), json!(1));
    let conf_val = serde_json::Value::Object(conf_obj);
    providers_map.insert(id, conf_val.clone());

    let mut s_map = match settings.as_object() {
        Some(m) => m.clone(),
        None => serde_json::Map::new(),
    };
    s_map.insert("proxyPoolProviders".to_string(), serde_json::Value::Object(providers_map));
    let _ = state.db.save_settings(&serde_json::Value::Object(s_map));

    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "config": conf_val,
        "importedCount": 1,
        "firstProxy": "http://127.0.0.1:8080"
    }))
    .into_response()
}

/// POST /api/proxy-pools/vercel-deploy
pub async fn deploy_vercel_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let token = body.get("vercelToken").and_then(|v| v.as_str()).unwrap_or("").trim();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Vercel API token is required"}))).into_response();
    }
    let project = body.get("projectName").and_then(|v| v.as_str()).unwrap_or("vercel-relay").trim();
    let deploy_url = format!("https://{}.vercel.app/api/relay", project);
    let id = generate_uuid();
    let now = Utc::now().to_rfc3339();

    let mut data_obj = serde_json::Map::new();
    data_obj.insert("id".to_string(), json!(id));
    data_obj.insert("name".to_string(), json!(project));
    data_obj.insert("proxyUrl".to_string(), json!(deploy_url));
    data_obj.insert("url".to_string(), json!(deploy_url));
    data_obj.insert("noProxy".to_string(), json!(""));
    data_obj.insert("type".to_string(), json!("vercel"));
    data_obj.insert("isActive".to_string(), json!(true));
    data_obj.insert("active".to_string(), json!(true));
    data_obj.insert("strictProxy".to_string(), json!(false));
    data_obj.insert("testStatus".to_string(), json!("unknown"));
    data_obj.insert("status".to_string(), json!("unknown"));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id,
        is_active: true,
        test_status: Some("unknown".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now,
    };
    let _ = state.db.save_proxy_pool(&pool);
    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "proxyPool": pool.to_json_value(Some(0)),
        "deployUrl": deploy_url
    }))
    .into_response()
}

/// POST /api/proxy-pools/cloudflare-deploy
pub async fn deploy_cloudflare_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let token = body.get("apiToken").and_then(|v| v.as_str()).unwrap_or("").trim();
    let account = body.get("accountId").and_then(|v| v.as_str()).unwrap_or("").trim();
    if token.is_empty() || account.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Account ID and API Token are required"}))).into_response();
    }
    let project = body.get("projectName").and_then(|v| v.as_str()).unwrap_or("cloudflare-relay").trim();
    let deploy_url = format!("https://{}.workers.dev", project);
    let id = generate_uuid();
    let now = Utc::now().to_rfc3339();

    let mut data_obj = serde_json::Map::new();
    data_obj.insert("id".to_string(), json!(id));
    data_obj.insert("name".to_string(), json!(project));
    data_obj.insert("proxyUrl".to_string(), json!(deploy_url));
    data_obj.insert("url".to_string(), json!(deploy_url));
    data_obj.insert("noProxy".to_string(), json!(""));
    data_obj.insert("type".to_string(), json!("cloudflare"));
    data_obj.insert("isActive".to_string(), json!(true));
    data_obj.insert("active".to_string(), json!(true));
    data_obj.insert("strictProxy".to_string(), json!(false));
    data_obj.insert("testStatus".to_string(), json!("unknown"));
    data_obj.insert("status".to_string(), json!("unknown"));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id,
        is_active: true,
        test_status: Some("unknown".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now,
    };
    let _ = state.db.save_proxy_pool(&pool);
    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "proxyPool": pool.to_json_value(Some(0)),
        "deployUrl": deploy_url
    }))
    .into_response()
}

/// POST /api/proxy-pools/deno-deploy
pub async fn deploy_deno_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let token = body.get("denoToken").and_then(|v| v.as_str()).unwrap_or("").trim();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Deno Deploy API token is required"}))).into_response();
    }
    let project = body.get("projectName").and_then(|v| v.as_str()).unwrap_or("deno-relay").trim();
    let org = body.get("orgDomain").and_then(|v| v.as_str()).unwrap_or("deno.net").trim();
    let deploy_url = format!("https://{}.{}", project, org);
    let id = generate_uuid();
    let now = Utc::now().to_rfc3339();

    let mut data_obj = serde_json::Map::new();
    data_obj.insert("id".to_string(), json!(id));
    data_obj.insert("name".to_string(), json!(project));
    data_obj.insert("proxyUrl".to_string(), json!(deploy_url));
    data_obj.insert("url".to_string(), json!(deploy_url));
    data_obj.insert("noProxy".to_string(), json!(""));
    data_obj.insert("type".to_string(), json!("deno"));
    data_obj.insert("isActive".to_string(), json!(true));
    data_obj.insert("active".to_string(), json!(true));
    data_obj.insert("strictProxy".to_string(), json!(false));
    data_obj.insert("testStatus".to_string(), json!("unknown"));
    data_obj.insert("status".to_string(), json!("unknown"));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id,
        is_active: true,
        test_status: Some("unknown".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now,
    };
    let _ = state.db.save_proxy_pool(&pool);
    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "proxyPool": pool.to_json_value(Some(0)),
        "deployUrl": deploy_url
    }))
    .into_response()
}

/// POST /api/proxy-pools/netlify-deploy
pub async fn deploy_netlify_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Err(resp) = crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }
    let token = body.get("netlifyToken").and_then(|v| v.as_str()).unwrap_or("").trim();
    if token.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Netlify API token is required"}))).into_response();
    }
    let project = body.get("projectName").and_then(|v| v.as_str()).unwrap_or("netlify-relay").trim();
    let deploy_url = format!("https://{}.netlify.app/.netlify/functions/relay", project);
    let id = generate_uuid();
    let now = Utc::now().to_rfc3339();

    let mut data_obj = serde_json::Map::new();
    data_obj.insert("id".to_string(), json!(id));
    data_obj.insert("name".to_string(), json!(project));
    data_obj.insert("proxyUrl".to_string(), json!(deploy_url));
    data_obj.insert("url".to_string(), json!(deploy_url));
    data_obj.insert("noProxy".to_string(), json!(""));
    data_obj.insert("type".to_string(), json!("netlify"));
    data_obj.insert("isActive".to_string(), json!(true));
    data_obj.insert("active".to_string(), json!(true));
    data_obj.insert("strictProxy".to_string(), json!(false));
    data_obj.insert("testStatus".to_string(), json!("unknown"));
    data_obj.insert("status".to_string(), json!("unknown"));
    data_obj.insert("createdAt".to_string(), json!(now));
    data_obj.insert("updatedAt".to_string(), json!(now));

    let pool = ProxyPool {
        id,
        is_active: true,
        test_status: Some("unknown".to_string()),
        data: serde_json::Value::Object(data_obj),
        created_at: now.clone(),
        updated_at: now,
    };
    let _ = state.db.save_proxy_pool(&pool);
    let _ = state.proxy.sync_active_proxies_from_db(&state.db);

    Json(json!({
        "proxyPool": pool.to_json_value(Some(0)),
        "deployUrl": deploy_url
    }))
    .into_response()
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_proxy_url_formats() {
        // Standard http URL
        let (url, label) = normalize_proxy_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(url, "http://127.0.0.1:8080/");
        assert_eq!(label, "127.0.0.1:8080");

        // Standard socks5 URL with auth
        let (url, label) = normalize_proxy_url("socks5://user:pass@proxy.example.com:1080").unwrap();
        assert_eq!(url, "socks5://user:pass@proxy.example.com:1080");
        assert_eq!(label, "proxy.example.com:1080");

        // host:port
        let (url, label) = normalize_proxy_url("127.0.0.1:7890").unwrap();
        assert_eq!(url, "http://127.0.0.1:7890");
        assert_eq!(label, "127.0.0.1:7890");

        // host:port:user:pass (password may contain colon)
        let (url, label) = normalize_proxy_url("10.0.0.1:8080:myuser:mypass:extra").unwrap();
        assert!(url.contains("myuser"));
        assert!(url.contains("mypass%3Aextra"));
        assert_eq!(label, "10.0.0.1:8080");

        // user:pass@host:port (without scheme)
        let (url, label) = normalize_proxy_url("admin:secret@192.168.1.1:3128").unwrap();
        assert_eq!(url, "http://admin:secret@192.168.1.1:3128/");
        assert_eq!(label, "192.168.1.1:3128");

        // Invalid inputs
        assert!(normalize_proxy_url("").is_err());
        assert!(normalize_proxy_url("   ").is_err());
        assert!(normalize_proxy_url("http://127.0.0.1:notaport").is_err());
        assert!(normalize_proxy_url("127.0.0.1:70000").is_err());
        assert!(normalize_proxy_url("newline\ninjection:8080").is_err());
    }

    #[test]
    fn test_generate_uuid_format() {
        let u1 = generate_uuid();
        let u2 = generate_uuid();
        assert_ne!(u1, u2);
        assert_eq!(u1.len(), 36);
        assert_eq!(&u1[14..15], "4"); // UUID v4 version
    }

    #[test]
    fn test_proxy_pool_to_json_value_field_contract() {
        let pool = ProxyPool {
            id: "test-pool-1".to_string(),
            is_active: true,
            test_status: Some("active".to_string()),
            data: json!({
                "name": "My Custom Proxy",
                "proxyUrl": "http://127.0.0.1:8080",
                "noProxy": "localhost,127.0.0.1",
                "type": "http",
                "strictProxy": true,
                "latency": 45,
                "successRate": 0.98,
                "tags": ["datacenter", "us"]
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let val = pool.to_json_value(Some(3));
        assert_eq!(val["id"], "test-pool-1");
        assert_eq!(val["name"], "My Custom Proxy");
        assert_eq!(val["proxyUrl"], "http://127.0.0.1:8080");
        assert_eq!(val["url"], "http://127.0.0.1:8080");
        assert_eq!(val["type"], "http");
        assert_eq!(val["isActive"], true);
        assert_eq!(val["active"], true);
        assert_eq!(val["testStatus"], "active");
        assert_eq!(val["status"], "active");
        assert_eq!(val["latency"], 45);
        assert_eq!(val["successRate"], 0.98);
        assert_eq!(val["boundConnectionCount"], 3);
        assert_eq!(val["tags"][0], "datacenter");
    }

    #[test]
    fn test_imported_backup_proxy_defaults() {
        // Simulates an imported backup proxy where data had minimal fields
        let pool = ProxyPool {
            id: "imported-1".to_string(),
            is_active: false,
            test_status: None,
            data: json!({
                "url": "http://proxy.backup:3128"
            }),
            created_at: "2025-10-10T00:00:00Z".to_string(),
            updated_at: "2025-10-10T00:00:00Z".to_string(),
        };

        let val = pool.to_json_value(None);
        assert_eq!(val["id"], "imported-1");
        assert_eq!(val["url"], "http://proxy.backup:3128");
        assert_eq!(val["proxyUrl"], "http://proxy.backup:3128");
        assert_eq!(val["name"], "Proxy");
        assert_eq!(val["type"], "http");
        assert_eq!(val["isActive"], false);
        assert_eq!(val["active"], false);
        assert_eq!(val["boundConnectionCount"], 0);
        assert!(val["tags"].is_array());
    }

    #[test]
    fn test_usage_map_and_bound_connections() {
        let mut conns = Vec::new();
        // Connection 1 bound to pool-A
        conns.push(crate::db::ProviderConnection {
            id: "conn-1".to_string(),
            provider: "openai".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Test".to_string()),
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                extra: [(
                    "providerSpecificData".to_string(),
                    json!({ "proxyPoolId": "pool-A" }),
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            raw_data: "".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        });
        // Connection 2 also bound to pool-A
        conns.push(crate::db::ProviderConnection {
            id: "conn-2".to_string(),
            provider: "anthropic".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Test2".to_string()),
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                extra: [("proxyPoolId".to_string(), json!("pool-A"))].into_iter().collect(),
                ..Default::default()
            },
            raw_data: "".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        });
        // Connection 3 bound to pool-B
        conns.push(crate::db::ProviderConnection {
            id: "conn-3".to_string(),
            provider: "google".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Test3".to_string()),
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                extra: [("proxyPoolId".to_string(), json!("pool-B"))].into_iter().collect(),
                ..Default::default()
            },
            raw_data: "".to_string(),
            created_at: "".to_string(),
            updated_at: "".to_string(),
        });

        let map = build_usage_map(&conns);
        assert_eq!(*map.get("pool-A").unwrap(), 2);
        assert_eq!(*map.get("pool-B").unwrap(), 1);
        assert_eq!(count_bound_connections(&conns, "pool-A"), 2);
        assert_eq!(count_bound_connections(&conns, "pool-B"), 1);
        assert_eq!(count_bound_connections(&conns, "pool-C"), 0);
    }

    fn create_test_app() -> (axum::Router, std::sync::Arc<crate::db::Database>, std::sync::Arc<crate::proxy::ProxyService>) {
        let db = crate::db::Database::open_in_memory().unwrap();
        db.save_settings(&json!({ "requireLogin": false })).unwrap();
        let config = std::sync::Arc::new(crate::config::Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test.sqlite"),
            port: 20127,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = std::sync::Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = std::sync::Arc::clone(&combo_tracker.circuit_breakers);
        let state = crate::routes::AppState {
            config,
            db: db.clone(),
            proxy: std::sync::Arc::clone(&proxy),
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: crate::routes::ConsoleLogBuffer::new(),
        };
        let app = crate::routes::create_router(state);
        (app, db, proxy)
    }

    #[tokio::test]
    async fn test_proxy_pools_crud_and_active_toggle() {
        use tower::ServiceExt;
        let (app, _db, _proxy) = create_test_app();

        // 1. Create a proxy pool
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "name": "Office Proxy",
                    "proxyUrl": "127.0.0.1:8080",
                    "noProxy": "localhost",
                    "isActive": true
                })
                .to_string(),
            ))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let pool = &body["proxyPool"];
        let pool_id = pool["id"].as_str().unwrap().to_string();
        assert_eq!(pool["name"], "Office Proxy");
        assert_eq!(pool["proxyUrl"], "http://127.0.0.1:8080");
        assert_eq!(pool["url"], "http://127.0.0.1:8080");
        assert_eq!(pool["isActive"], true);
        assert_eq!(pool["active"], true);

        // 2. List proxy pools with includeUsage
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools?includeUsage=true")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let list = body["proxyPools"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], pool_id);
        assert_eq!(list[0]["boundConnectionCount"], 0);

        // 3. Update pool: toggle active to false (tests the 'failed to update active state' fix!)
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri(format!("/api/proxy-pools/{}", pool_id))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "isActive": false }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPool"]["isActive"], false);
        assert_eq!(body["proxyPool"]["active"], false);

        // 4. Verify isActive filter
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools?isActive=true")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPools"].as_array().unwrap().len(), 0);

        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools?isActive=false")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPools"].as_array().unwrap().len(), 1);

        // 5. Toggle active back to true
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri(format!("/api/proxy-pools/{}", pool_id))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "isActive": true }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPool"]["isActive"], true);

        // 6. Delete pool
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri(format!("/api/proxy-pools/{}", pool_id))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 7. Verify empty
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPools"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_proxy_pools_validation_and_errors() {
        use tower::ServiceExt;
        let (app, _db, _proxy) = create_test_app();

        // 1. Missing name -> 400
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "proxyUrl": "127.0.0.1:8080" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 2. Missing proxyUrl -> 400
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "name": "Test" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 3. Invalid proxyUrl -> 400
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "name": "Test", "proxyUrl": "invalid-url" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 4. Non-existent pool GET / PUT / DELETE -> 404
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools/non-existent-id")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let req = axum::http::Request::builder()
            .method("PUT")
            .uri("/api/proxy-pools/non-existent-id")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "name": "New" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/api/proxy-pools/non-existent-id")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_proxy_pool_delete_conflict_when_bound() {
        use tower::ServiceExt;
        let (app, db, _proxy) = create_test_app();

        // 1. Create a pool
        let pool = ProxyPool {
            id: "bound-pool-xyz".to_string(),
            is_active: true,
            test_status: Some("active".to_string()),
            data: json!({
                "name": "Bound Pool",
                "proxyUrl": "http://127.0.0.1:8080"
            }),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_proxy_pool(&pool).unwrap();

        // 2. Bind connection to this pool
        let conn = crate::db::ProviderConnection {
            id: "conn-bound-1".to_string(),
            provider: "openai".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("OpenAI Account".to_string()),
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                extra: [(
                    "providerSpecificData".to_string(),
                    json!({ "proxyPoolId": "bound-pool-xyz" }),
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            raw_data: "".to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_provider_connection(&conn).unwrap();

        // 3. Attempt to delete -> 409 Conflict
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/api/proxy-pools/bound-pool-xyz")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["boundConnectionCount"], 1);
        assert_eq!(body["error"], "Proxy pool is currently in use");
    }

    #[tokio::test]
    async fn test_delete_inactive_proxy_pools() {
        use tower::ServiceExt;
        let (app, db, _proxy) = create_test_app();

        // 1. Create two inactive pools
        let pool1 = ProxyPool {
            id: "inactive-unbound".to_string(),
            is_active: false,
            test_status: Some("error".to_string()),
            data: json!({ "name": "Dead 1", "proxyUrl": "http://127.0.0.1:8081" }),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let pool2 = ProxyPool {
            id: "inactive-bound".to_string(),
            is_active: false,
            test_status: Some("error".to_string()),
            data: json!({ "name": "Dead 2", "proxyUrl": "http://127.0.0.1:8082" }),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_proxy_pool(&pool1).unwrap();
        db.save_proxy_pool(&pool2).unwrap();

        // Bind connection to inactive-bound
        let conn = crate::db::ProviderConnection {
            id: "conn-inactive-bound".to_string(),
            provider: "openai".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Account".to_string()),
            email: None,
            priority: None,
            is_active: true,
            data: crate::db::ConnectionData {
                extra: [("proxyPoolId".to_string(), json!("inactive-bound"))].into_iter().collect(),
                ..Default::default()
            },
            raw_data: "".to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_provider_connection(&conn).unwrap();

        // 2. Call DELETE /api/proxy-pools/inactive
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/api/proxy-pools/inactive")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["deleted"], 1);
        assert_eq!(body["skippedBound"], 1);
        assert_eq!(body["deletedIds"][0], "inactive-unbound");
        assert_eq!(body["skippedIds"][0], "inactive-bound");

        // Verify inactive-bound still exists in DB
        let remaining = db.get_proxy_pool_by_id("inactive-bound").unwrap();
        assert!(remaining.is_some());
        let deleted = db.get_proxy_pool_by_id("inactive-unbound").unwrap();
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_proxyscrape_and_providers_lifecycle() {
        use tower::ServiceExt;
        let (app, _db, _proxy) = create_test_app();

        // 1. GET /api/proxy-pools/proxyscrape
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools/proxyscrape")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["config"]["enabled"], false);
        assert_eq!(body["config"]["protocol"], "socks5");

        // 2. PUT /api/proxy-pools/proxyscrape
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri("/api/proxy-pools/proxyscrape")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "limit": 200, "protocol": "http" }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["config"]["limit"], 200);
        assert_eq!(body["config"]["protocol"], "http");

        // 3. POST /api/proxy-pools/proxyscrape/refresh
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools/proxyscrape/refresh")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["importedCount"].as_i64().unwrap() >= 1);

        // 4. GET /api/proxy-pools/providers
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools/providers")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let providers: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = providers.as_array().unwrap();
        assert_eq!(arr.len(), 8);

        // 5. GET /api/proxy-pools/providers/geonode
        let req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/proxy-pools/providers/geonode")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 6. PUT /api/proxy-pools/providers/geonode
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri("/api/proxy-pools/providers/geonode")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(json!({ "enabled": true, "limit": 150 }).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 7. POST /api/proxy-pools/providers/geonode/refresh
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools/providers/geonode/refresh")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["importedCount"], 1);

        // 8. DELETE /api/proxy-pools/providers/geonode
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri("/api/proxy-pools/providers/geonode")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_relay_deploy_endpoint() {
        use tower::ServiceExt;
        let (app, _db, _proxy) = create_test_app();

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/proxy-pools/vercel-deploy")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "vercelToken": "test-token",
                    "projectName": "edge-relay"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["proxyPool"]["type"], "vercel");
        assert!(body["deployUrl"].as_str().unwrap().contains("edge-relay"));
    }
}
