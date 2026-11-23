use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use url::Url;

use crate::{
    db::{ApiKey, Database},
    routes::AppState,
};

// ============================================================================
// Types and Schemas
// ============================================================================

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebFetchRequest {
    pub url: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub max_characters: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchMetadata {
    pub title: Option<String>,
    #[serde(rename = "sourceURL")]
    pub source_url: String,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchData {
    pub markdown: String,
    pub metadata: WebFetchMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchResponse {
    pub success: bool,
    pub data: WebFetchData,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebSearchRequest {
    pub query: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchResponse {
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemOneRequest {
    pub model: String,
    pub state: Value,
    #[serde(default)]
    pub questions: Option<Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemOneResponse {
    pub decision: String,
    pub confidence: f64,
    pub metadata: Value,
}

// ============================================================================
// JSON Response Helpers
// ============================================================================

pub fn json_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        json!({ "error": message }).to_string(),
    )
        .into_response()
}

pub fn json_success<T: Serialize>(value: &T) -> Response {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body,
    )
        .into_response()
}

// ============================================================================
// SSRF Guard Logic
// ============================================================================

/// Checks if an IPv4 address belongs to a private, loopback, link-local,
/// CGNAT, documentation, multicast, or reserved range.
pub fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    // 0.0.0.0/8 (Current network / "this host")
    if octets[0] == 0 {
        return true;
    }
    // 10.0.0.0/8 (Private network RFC 1918)
    if octets[0] == 10 {
        return true;
    }
    // 100.64.0.0/10 (CGNAT RFC 6598)
    if octets[0] == 100 && (octets[1] & 0xc0) == 64 {
        return true;
    }
    // 127.0.0.0/8 (Loopback RFC 1122)
    if octets[0] == 127 {
        return true;
    }
    // 169.254.0.0/16 (Link-local RFC 3927, includes cloud metadata 169.254.169.254)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }
    // 172.16.0.0/12 (Private network RFC 1918)
    if octets[0] == 172 && (octets[1] & 0xf0) == 16 {
        return true;
    }
    // 192.0.0.0/24 (IETF protocol assignments)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return true;
    }
    // 192.0.2.0/24 (TEST-NET-1)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return true;
    }
    // 192.88.99.0/24 (6to4 relay anycast)
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return true;
    }
    // 192.168.0.0/16 (Private network RFC 1918)
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    // 198.18.0.0/15 (Benchmarking)
    if octets[0] == 198 && (octets[1] & 0xfe) == 18 {
        return true;
    }
    // 198.51.100.0/24 (TEST-NET-2)
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return true;
    }
    // 203.0.113.0/24 (TEST-NET-3)
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return true;
    }
    // 224.0.0.0/4 (Multicast RFC 5771)
    if octets[0] >= 224 && octets[0] <= 239 {
        return true;
    }
    // 240.0.0.0/4 (Reserved / Broadcast RFC 1112)
    if octets[0] >= 240 {
        return true;
    }

    false
}

/// Checks if an IPv6 address belongs to a loopback, unspecified, unique local (ULA),
/// link-local, multicast, documentation, or IPv4-mapped private range.
pub fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();

    // ::1 (Loopback)
    if ip.is_loopback() {
        return true;
    }
    // :: (Unspecified)
    if ip.is_unspecified() {
        return true;
    }
    // fc00::/7 (Unique Local Address - ULA)
    if (seg[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // fe80::/10 (Link-Local Unicast)
    if (seg[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // ff00::/8 (Multicast)
    if (seg[0] & 0xff00) == 0xff00 {
        return true;
    }
    // 2001:db8::/32 (Documentation)
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true;
    }
    // 100::/64 (Discard prefix)
    if seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 {
        return true;
    }
    // ::ffff:0:0/96 (IPv4-mapped IPv6)
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 && seg[5] == 0xffff {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return is_blocked_ipv4(v4);
    }
    // 2002::/16 (6to4)
    if seg[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            (seg[1] & 0xff) as u8,
            (seg[2] >> 8) as u8,
            (seg[2] & 0xff) as u8,
        );
        return is_blocked_ipv4(v4);
    }

    false
}

/// Checks if an IP address (IPv4 or IPv6) is blocked for SSRF prevention.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// Checks if a hostname matches blocked internal/local names or suffixes.
pub fn is_blocked_hostname(host: &str) -> bool {
    let clean = host.trim().trim_end_matches('.').to_ascii_lowercase();

    if clean.is_empty() {
        return true;
    }

    // Exact matches
    if clean == "localhost"
        || clean == "ip6-localhost"
        || clean == "ip6-loopback"
        || clean == "broadcasthost"
        || clean == "local"
    {
        return true;
    }

    // Suffix checks
    if clean.ends_with(".localhost")
        || clean.ends_with(".local")
        || clean.ends_with(".internal")
        || clean.ends_with(".lan")
        || clean.ends_with(".home")
        || clean.ends_with(".corp")
    {
        return true;
    }

    false
}

/// Validates URL protocol, hostname syntax, and literal IP checks.
pub fn validate_url_syntax(url_str: &str) -> Result<Url, String> {
    let parsed = Url::parse(url_str).map_err(|e| format!("Invalid URL format: {}", e))?;

    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("Unsupported protocol: '{}'. Only http and https are allowed.", scheme));
    }

    let host_str = match parsed.host_str() {
        Some(h) if !h.trim().is_empty() => h.trim(),
        _ => return Err("URL must contain a valid host".to_string()),
    };

    if is_blocked_hostname(host_str) {
        return Err(format!("Blocked URL: hostname '{}' is not allowed", host_str));
    }

    // If host is an IP literal, validate directly
    if let Ok(ip) = host_str.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(format!("Blocked URL: destination IP '{}' is private or loopback", ip));
        }
    }

    Ok(parsed)
}

/// Fully validates a URL including asynchronous DNS resolution of the hostname.
/// Rejects if any resolved IP address belongs to a private/loopback/internal range.
pub async fn assert_public_url_resolved(url_str: &str) -> Result<Url, String> {
    let parsed = validate_url_syntax(url_str)?;
    let host = parsed.host_str().unwrap();
    let port = parsed.port_or_known_default().unwrap_or(80);

    // If it's already an IP literal, syntax check already verified it
    if host.parse::<IpAddr>().is_ok() {
        return Ok(parsed);
    }

    // Resolve domain through DNS
    let host_port = format!("{}:{}", host, port);
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host(&host_port).await {
        Ok(iter) => iter.collect(),
        Err(e) => return Err(format!("DNS resolution failed for '{}': {}", host, e)),
    };

    if addrs.is_empty() {
        return Err(format!("No IP addresses found for host '{}'", host));
    }

    for sa in addrs {
        if is_blocked_ip(sa.ip()) {
            return Err(format!(
                "Blocked URL: host '{}' resolves to private/loopback address '{}'",
                host,
                sa.ip()
            ));
        }
    }

    Ok(parsed)
}

// ============================================================================
// HTML Parsing and Markdown Extraction
// ============================================================================

/// Decodes standard HTML entities into text.
pub fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '&' {
            let mut entity = String::new();
            while let Some(&next_ch) = chars.peek() {
                if next_ch == ';' {
                    chars.next();
                    break;
                }
                if next_ch.is_alphanumeric() || next_ch == '#' {
                    entity.push(chars.next().unwrap());
                    if entity.len() > 10 {
                        break;
                    }
                } else {
                    break;
                }
            }

            match entity.as_str() {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                "nbsp" => out.push(' '),
                "copy" => out.push('©'),
                "reg" => out.push('®'),
                "mdash" => out.push('—'),
                "ndash" => out.push('–'),
                _ => {
                    if let Some(stripped) = entity.strip_prefix('#') {
                        if let Some(hex) = stripped.strip_prefix('x').or_else(|| stripped.strip_prefix('X')) {
                            if let Ok(code) = u32::from_str_radix(hex, 16) {
                                if let Some(c) = char::from_u32(code) {
                                    out.push(c);
                                    continue;
                                }
                            }
                        } else if let Ok(code) = stripped.parse::<u32>() {
                            if let Some(c) = char::from_u32(code) {
                                out.push(c);
                                continue;
                            }
                        }
                    }
                    out.push('&');
                    out.push_str(&entity);
                }
            }
        } else {
            out.push(ch);
        }
    }

    out
}

/// Extracts page title from `<title>` tag or `<h1>` heading.
pub fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();

    // Check <title>
    if let Some(start_tag) = lower.find("<title") {
        if let Some(open_end) = lower[start_tag..].find('>') {
            let content_start = start_tag + open_end + 1;
            if let Some(close_tag) = lower[content_start..].find("</title>") {
                let raw_title = &html[content_start..content_start + close_tag];
                let decoded = decode_html_entities(raw_title).trim().to_string();
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }

    // Fallback to first <h1>
    if let Some(start_tag) = lower.find("<h1") {
        if let Some(open_end) = lower[start_tag..].find('>') {
            let content_start = start_tag + open_end + 1;
            if let Some(close_tag) = lower[content_start..].find("</h1>") {
                let raw_h1 = &html[content_start..content_start + close_tag];
                let decoded = decode_html_entities(raw_h1).trim().to_string();
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }

    None
}

/// Converts HTML into clean, human-readable Markdown or text.
pub fn html_to_markdown(html: &str, format: &str, max_characters: Option<usize>) -> String {
    if format.eq_ignore_ascii_case("html") {
        return truncate_text(html, max_characters);
    }

    let mut result = String::with_capacity(html.len());
    let mut pos = 0;
    let bytes = html.as_bytes();
    let len = bytes.len();

    while pos < len {
        // Check for tags
        if bytes[pos] == b'<' {
            let remaining = &html[pos..];

            // 1. Comments <!-- ... -->
            if remaining.starts_with("<!--") {
                if let Some(end) = remaining.find("-->") {
                    pos += end + 3;
                    continue;
                }
            }

            // 2. <script ... </script>
            if remaining.len() >= 7 && remaining[..7].eq_ignore_ascii_case("<script") {
                let lower = remaining.to_ascii_lowercase();
                if let Some(end) = lower.find("</script>") {
                    pos += end + 9;
                    continue;
                }
            }

            // 3. <style ... </style>
            if remaining.len() >= 6 && remaining[..6].eq_ignore_ascii_case("<style") {
                let lower = remaining.to_ascii_lowercase();
                if let Some(end) = lower.find("</style>") {
                    pos += end + 8;
                    continue;
                }
            }

            // 4. <noscript ... </noscript>
            if remaining.len() >= 9 && remaining[..9].eq_ignore_ascii_case("<noscript") {
                let lower = remaining.to_ascii_lowercase();
                if let Some(end) = lower.find("</noscript>") {
                    pos += end + 11;
                    continue;
                }
            }

            // Find tag end '>'
            if let Some(tag_end_rel) = remaining.find('>') {
                let tag_str = &remaining[1..tag_end_rel];
                let tag_name = tag_str
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_ascii_lowercase();

                let is_closing = tag_str.trim_start().starts_with('/');

                match tag_name.as_str() {
                    "h1" => {
                        if !is_closing {
                            result.push_str("\n\n# ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "h2" => {
                        if !is_closing {
                            result.push_str("\n\n## ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "h3" => {
                        if !is_closing {
                            result.push_str("\n\n### ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "h4" => {
                        if !is_closing {
                            result.push_str("\n\n#### ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "h5" => {
                        if !is_closing {
                            result.push_str("\n\n##### ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "h6" => {
                        if !is_closing {
                            result.push_str("\n\n###### ");
                        } else {
                            result.push('\n');
                        }
                    }
                    "p" | "div" | "article" | "section" => {
                        result.push_str("\n\n");
                    }
                    "br" => {
                        result.push('\n');
                    }
                    "hr" => {
                        result.push_str("\n\n---\n\n");
                    }
                    "li" => {
                        if !is_closing {
                            result.push_str("\n* ");
                        }
                    }
                    "b" | "strong" => {
                        result.push_str("**");
                    }
                    "i" | "em" => {
                        result.push('*');
                    }
                    "code" => {
                        result.push('`');
                    }
                    "pre" => {
                        result.push_str("\n```\n");
                    }
                    "a" => {
                        // Keep simple: links will have their inner text rendered
                    }
                    _ => {}
                }

                pos += tag_end_rel + 1;
                continue;
            }
        }

        // Regular character
        result.push(bytes[pos] as char);
        pos += 1;
    }

    let decoded = decode_html_entities(&result);

    // Clean up whitespace: compress multiple empty lines to max 2
    let mut cleaned = String::with_capacity(decoded.len());
    let mut consecutive_newlines = 0;

    for line in decoded.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 && !cleaned.is_empty() {
                cleaned.push('\n');
            }
        } else {
            consecutive_newlines = 0;
            if !cleaned.is_empty() && !cleaned.ends_with('\n') {
                cleaned.push('\n');
            }
            cleaned.push_str(trimmed_line);
        }
    }

    let trimmed = cleaned.trim().to_string();
    truncate_text(&trimmed, max_characters)
}

fn truncate_text(text: &str, max_characters: Option<usize>) -> String {
    match max_characters {
        Some(max) if max > 0 && text.chars().count() > max => {
            text.chars().take(max).collect()
        }
        _ => text.to_string(),
    }
}

// ============================================================================
// Web Fetch Implementation
// ============================================================================

/// Native reqwest fetch with SSRF validation and safe redirect handling.
pub async fn execute_native_fetch(
    url_str: &str,
    format_opt: Option<&str>,
    max_characters: Option<usize>,
) -> Result<WebFetchData, (StatusCode, String)> {
    let mut current_url = assert_public_url_resolved(url_str)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut hops = 0;
    const MAX_HOPS: usize = 5;

    let response_body = loop {
        if hops >= MAX_HOPS {
            return Err((StatusCode::BAD_REQUEST, "Too many redirects".to_string()));
        }

        let resp = client
            .get(current_url.as_str())
            .header(header::USER_AGENT, "Mozilla/5.0 (compatible; 9Router/0.6; +https://9router.com)")
            .header(header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.7")
            .send()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Fetch failed: {}", e)))?;

        let status = resp.status();
        if status.is_redirection() {
            if let Some(loc) = resp.headers().get(header::LOCATION) {
                let loc_str = loc.to_str().map_err(|_| {
                    (StatusCode::BAD_GATEWAY, "Invalid redirect Location header".to_string())
                })?;
                let next_url = current_url
                    .join(loc_str)
                    .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Invalid redirect target: {}", e)))?;

                // Re-verify the redirect URL with full SSRF and DNS check
                current_url = assert_public_url_resolved(next_url.as_str())
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("Redirect blocked: {}", e)))?;

                hops += 1;
                continue;
            } else {
                return Err((StatusCode::BAD_GATEWAY, "Redirect missing Location header".to_string()));
            }
        }

        if !status.is_success() {
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("Upstream returned HTTP status {}", status.as_u16()),
            ));
        }

        let body_bytes = resp
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Failed reading response: {}", e)))?;

        break String::from_utf8_lossy(&body_bytes).to_string();
    };

    let title = extract_title(&response_body);
    let format = format_opt.unwrap_or("markdown");
    let markdown = html_to_markdown(&response_body, format, max_characters);

    Ok(WebFetchData {
        markdown,
        metadata: WebFetchMetadata {
            title,
            source_url: current_url.to_string(),
            extra: HashMap::new(),
        },
    })
}

/// Firecrawl scraper caller (`https://api.firecrawl.dev/v1/scrape`).
pub async fn execute_firecrawl_scrape(
    client: &reqwest::Client,
    api_key: &str,
    base_url_opt: Option<&str>,
    url: &str,
    format_opt: Option<&str>,
    max_characters: Option<usize>,
) -> Result<WebFetchData, (StatusCode, String)> {
    let endpoint = base_url_opt
        .unwrap_or("https://api.firecrawl.dev")
        .trim_end_matches('/');
    let target = format!("{}/v1/scrape", endpoint);

    let format = format_opt.unwrap_or("markdown");
    let payload = json!({
        "url": url,
        "formats": [format]
    });

    let resp = client
        .post(&target)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl request failed: {}", e)))?;

    let status = resp.status();
    let body_bytes = resp
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl read error: {}", e)))?;

    if !status.is_success() {
        let err_text = String::from_utf8_lossy(&body_bytes);
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Firecrawl error ({}): {}", status.as_u16(), err_text),
        ));
    }

    let parsed: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl JSON error: {}", e)))?;

    let data_obj = parsed.get("data").cloned().unwrap_or(Value::Null);

    let raw_content = data_obj
        .get("markdown")
        .and_then(|v| v.as_str())
        .or_else(|| data_obj.get("html").and_then(|v| v.as_str()))
        .or_else(|| data_obj.get("text").and_then(|v| v.as_str()))
        .unwrap_or("");

    let title = data_obj
        .get("metadata")
        .and_then(|m| m.get("title"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());

    let source_url = data_obj
        .get("metadata")
        .and_then(|m| m.get("sourceURL"))
        .and_then(|s| s.as_str())
        .unwrap_or(url)
        .to_string();

    let content = truncate_text(raw_content, max_characters);

    Ok(WebFetchData {
        markdown: content,
        metadata: WebFetchMetadata {
            title,
            source_url,
            extra: HashMap::new(),
        },
    })
}

// ============================================================================
// Web Search Implementation
// ============================================================================

/// Normalizes Firecrawl search response into unified SearchResults.
pub fn parse_firecrawl_search_response(json: &Value, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    let items = if let Some(arr) = json.get("data").and_then(|v| v.as_array()) {
        arr
    } else if let Some(arr) = json.get("results").and_then(|v| v.as_array()) {
        arr
    } else {
        return results;
    };

    for item in items {
        if results.len() >= limit {
            break;
        }

        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let snippet = item
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("markdown").and_then(|v| v.as_str()))
            .or_else(|| item.get("snippet").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();

        if !url.is_empty() || !title.is_empty() {
            results.push(SearchResult {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                snippet,
            });
        }
    }

    results
}

/// Normalizes DuckDuckGo Instant Answer API response into unified SearchResults.
pub fn parse_duckduckgo_instant_api(val: &Value, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // 1. Primary abstract result
    let abstract_text = val.get("AbstractText").and_then(|v| v.as_str()).unwrap_or("").trim();
    let abstract_url = val.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or("").trim();
    let heading = val.get("Heading").and_then(|v| v.as_str()).unwrap_or("").trim();

    if !abstract_text.is_empty() && !abstract_url.is_empty() {
        results.push(SearchResult {
            title: if heading.is_empty() {
                abstract_url.to_string()
            } else {
                heading.to_string()
            },
            url: abstract_url.to_string(),
            snippet: abstract_text.to_string(),
        });
    }

    // 2. RelatedTopics
    if let Some(topics) = val.get("RelatedTopics").and_then(|v| v.as_array()) {
        for item in topics {
            if results.len() >= limit {
                break;
            }

            // Could be nested topics array
            if let Some(sub_topics) = item.get("Topics").and_then(|v| v.as_array()) {
                for sub in sub_topics {
                    if results.len() >= limit {
                        break;
                    }
                    if let Some(sr) = extract_ddg_topic(sub) {
                        results.push(sr);
                    }
                }
                continue;
            }

            if let Some(sr) = extract_ddg_topic(item) {
                results.push(sr);
            }
        }
    }

    results
}

fn extract_ddg_topic(item: &Value) -> Option<SearchResult> {
    let url = item.get("FirstURL").and_then(|v| v.as_str()).unwrap_or("").trim();
    let text = item.get("Text").and_then(|v| v.as_str()).unwrap_or("").trim();

    if url.is_empty() || text.is_empty() {
        return None;
    }

    let title = if let Some((left, _)) = text.split_once(" - ") {
        left.trim().to_string()
    } else {
        text.chars().take(80).collect()
    };

    Some(SearchResult {
        title,
        url: url.to_string(),
        snippet: text.to_string(),
    })
}

/// Normalizes DuckDuckGo HTML search results.
pub fn parse_duckduckgo_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut search_pos = 0;

    while search_pos < html.len() {
        if results.len() >= limit {
            break;
        }

        // Look for result block or title link: <a class="result__url" href="..."> or <a class="result__a" href="...">
        let marker = "<a class=\"result__a\"";
        let found = match lower[search_pos..].find(marker) {
            Some(idx) => search_pos + idx,
            None => break,
        };

        // Extract href
        let after_marker = &html[found..];
        let href = if let Some(href_start) = after_marker.find("href=\"") {
            let start = href_start + 6;
            if let Some(href_end) = after_marker[start..].find('"') {
                let raw_href = &after_marker[start..start + href_end];
                // Unescape DuckDuckGo redirect wrapper if present
                clean_ddg_url(raw_href)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Extract title text within this <a>...</a>
        let title = if let Some(tag_end) = after_marker.find('>') {
            let content_start = tag_end + 1;
            if let Some(close_tag) = after_marker[content_start..].find("</a>") {
                let raw_title = &after_marker[content_start..content_start + close_tag];
                strip_html_tags(raw_title)
            } else {
                href.clone()
            }
        } else {
            href.clone()
        };

        // Extract snippet if present nearby
        let snippet_marker = "class=\"result__snippet\"";
        let snippet = if let Some(snip_idx) = after_marker.find(snippet_marker) {
            let snip_part = &after_marker[snip_idx..];
            if let Some(tag_end) = snip_part.find('>') {
                let s_start = tag_end + 1;
                if let Some(s_end) = snip_part[s_start..].find("</a>") {
                    strip_html_tags(&snip_part[s_start..s_start + s_end])
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !href.is_empty() {
            results.push(SearchResult {
                title: if title.is_empty() { href.clone() } else { title },
                url: href,
                snippet,
            });
        }

        search_pos = found + marker.len() + 10;
    }

    results
}

fn clean_ddg_url(raw_url: &str) -> String {
    if let Some(pos) = raw_url.find("uddg=") {
        let after = &raw_url[pos + 5..];
        let enc = after.split('&').next().unwrap_or("");
        if let Ok(decoded) = percent_encoding::percent_decode_str(enc).decode_utf8() {
            return decoded.to_string();
        }
    }
    raw_url.to_string()
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(ch);
        }
    }
    decode_html_entities(&out).trim().to_string()
}

/// Firecrawl Search caller (`https://api.firecrawl.dev/v1/search`).
pub async fn execute_firecrawl_search(
    client: &reqwest::Client,
    api_key: &str,
    base_url_opt: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, (StatusCode, String)> {
    let endpoint = base_url_opt
        .unwrap_or("https://api.firecrawl.dev")
        .trim_end_matches('/');
    let target = format!("{}/v1/search", endpoint);

    let payload = json!({
        "query": query,
        "limit": limit
    });

    let resp = client
        .post(&target)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl search failed: {}", e)))?;

    let status = resp.status();
    let body_bytes = resp
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl read error: {}", e)))?;

    if !status.is_success() {
        let err_text = String::from_utf8_lossy(&body_bytes);
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Firecrawl error ({}): {}", status.as_u16(), err_text),
        ));
    }

    let parsed: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Firecrawl JSON error: {}", e)))?;

    Ok(parse_firecrawl_search_response(&parsed, limit))
}

/// Fallback DuckDuckGo search (Instant Answer API + HTML fallback).
pub async fn execute_duckduckgo_search(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, (StatusCode, String)> {
    let encoded_query = urlencoding::encode(query);

    // 1. Try DuckDuckGo Instant Answer API
    let instant_url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        encoded_query
    );

    if let Ok(resp) = client
        .get(&instant_url)
        .header(header::USER_AGENT, "Mozilla/5.0 (compatible; 9Router/0.6; +https://9router.com)")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(bytes) = resp.bytes().await {
                if let Ok(val) = serde_json::from_slice::<Value>(&bytes) {
                    let results = parse_duckduckgo_instant_api(&val, limit);
                    if !results.is_empty() {
                        return Ok(results);
                    }
                }
            }
        }
    }

    // 2. Fallback to DuckDuckGo HTML search
    let html_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);
    let resp = client
        .get(&html_url)
        .header(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("DuckDuckGo search failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("DuckDuckGo search returned HTTP {}", resp.status().as_u16()),
        ));
    }

    let html_bytes = resp
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Failed reading DuckDuckGo response: {}", e)))?;

    let html_str = String::from_utf8_lossy(&html_bytes);
    let results = parse_duckduckgo_html(&html_str, limit);

    Ok(results)
}

// Simple urlencoding helper
mod urlencoding {
    pub fn encode(data: &str) -> String {
        percent_encoding::utf8_percent_encode(data, percent_encoding::NON_ALPHANUMERIC).to_string()
    }
}

// ============================================================================
// System One (Decision Evaluation)
// ============================================================================

/// Evaluates prompt state text and questions/criteria to produce an intelligent decision.
pub fn evaluate_system_one_decision(req: &SystemOneRequest) -> SystemOneResponse {
    let state_text = extract_text_from_state(&req.state);
    let lower_state = state_text.to_ascii_lowercase();

    // Check if questions contain a route/choice criteria
    if let Some(questions) = &req.questions {
        if let Some(route_obj) = questions.get("route").or_else(|| questions.get("choice")) {
            if let Some(criteria) = route_obj.get("criteria").and_then(|v| v.as_object()) {
                let candidates: Vec<&String> = criteria.keys().collect();
                if !candidates.is_empty() {
                    // Match candidate based on keywords in state
                    let is_coding = lower_state.contains("code")
                        || lower_state.contains("fn ")
                        || lower_state.contains("def ")
                        || lower_state.contains("class ")
                        || lower_state.contains("import ")
                        || lower_state.contains("struct ")
                        || lower_state.contains("impl ")
                        || lower_state.contains("async ")
                        || lower_state.contains("bug")
                        || lower_state.contains("compile")
                        || lower_state.contains("rust")
                        || lower_state.contains("python");

                    let is_reasoning = lower_state.contains("solve")
                        || lower_state.contains("calculate")
                        || lower_state.contains("math")
                        || lower_state.contains("proof")
                        || lower_state.contains("reason")
                        || lower_state.contains("step by step")
                        || lower_state.contains("why");

                    let mut chosen = candidates[0].clone();
                    let mut confidence = 0.90;

                    if is_coding {
                        if let Some(coder_model) = candidates.iter().find(|m| {
                            let ml = m.to_ascii_lowercase();
                            ml.contains("code") || ml.contains("coder") || ml.contains("sonnet") || ml.contains("deepseek")
                        }) {
                            chosen = (*coder_model).clone();
                            confidence = 0.96;
                        }
                    } else if is_reasoning {
                        if let Some(reason_model) = candidates.iter().find(|m| {
                            let ml = m.to_ascii_lowercase();
                            ml.contains("r1") || ml.contains("o1") || ml.contains("o3") || ml.contains("reason") || ml.contains("think")
                        }) {
                            chosen = (*reason_model).clone();
                            confidence = 0.94;
                        }
                    }

                    return SystemOneResponse {
                        decision: chosen,
                        confidence,
                        metadata: json!({
                            "model": req.model,
                            "intent": if is_coding { "coding" } else if is_reasoning { "reasoning" } else { "general" },
                            "candidates_count": candidates.len(),
                            "evaluated_by": "system_one_criteria_router"
                        }),
                    };
                }
            }
        }
    }

    // Heuristic categorization when no explicit criteria map is passed
    let (decision, confidence, intent) = if lower_state.contains("code")
        || lower_state.contains("fn ")
        || lower_state.contains("def ")
        || lower_state.contains("class ")
        || lower_state.contains("import ")
    {
        ("coding", 0.96, "code_generation")
    } else if lower_state.contains("solve")
        || lower_state.contains("calculate")
        || lower_state.contains("math")
        || lower_state.contains("reason")
    {
        ("reasoning", 0.94, "complex_reasoning")
    } else if lower_state.contains("extract")
        || lower_state.contains("summary")
        || lower_state.contains("json")
    {
        ("extraction", 0.91, "data_extraction")
    } else {
        ("general", 0.88, "chat_completion")
    };

    SystemOneResponse {
        decision: decision.to_string(),
        confidence,
        metadata: json!({
            "model": req.model,
            "intent": intent,
            "state_length": state_text.len(),
            "evaluated_by": "system_one_native_evaluator"
        }),
    }
}

fn extract_text_from_state(val: &Value) -> String {
    if let Some(s) = val.as_str() {
        return s.to_string();
    }

    // If array of messages
    if let Some(arr) = val.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
                parts.push(content);
            } else if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                parts.push(text);
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }

    // If object with messages
    if let Some(msgs) = val.get("messages").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for item in msgs {
            if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
                parts.push(content);
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }

    val.to_string()
}

// ============================================================================
// Auth Verification Helper
// ============================================================================

/// Validates API key if provided, or checks whether settings enforce API keys.
pub fn authenticate_client(
    headers: &HeaderMap,
    db: &Database,
) -> Result<Option<ApiKey>, Response> {
    if let Some(raw_key) = crate::auth::extract_api_key(headers) {
        match db.get_api_key_by_key(&raw_key) {
            Ok(Some(k)) => {
                if db.validate_key(&k).is_ok() {
                    return Ok(Some(k));
                } else {
                    return Err(json_error(StatusCode::UNAUTHORIZED, "Invalid API key"));
                }
            }
            _ => return Err(json_error(StatusCode::UNAUTHORIZED, "Invalid API key")),
        }
    }

    // No API key provided in request headers: check if settings enforce requireApiKey
    let require_key = db
        .load_settings()
        .ok()
        .and_then(|s| s.get("requireApiKey").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    if require_key {
        return Err(json_error(StatusCode::UNAUTHORIZED, "API key required"));
    }

    Ok(None)
}

// ============================================================================
// Axum HTTP Handlers
// ============================================================================

/// Handler for POST /api/v1/web/fetch and POST /v1/web/fetch
pub async fn web_fetch_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(auth_resp) = authenticate_client(&headers, &state.db) {
        return auth_resp;
    }

    let req: WebFetchRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    if req.url.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing required field: url");
    }

    let provider_name = req
        .provider
        .as_deref()
        .or(req.model.as_deref())
        .unwrap_or("");

    // Check if Firecrawl is configured in database
    let firecrawl_connections = state
        .db
        .get_active_provider_connections(Some("firecrawl"))
        .unwrap_or_default();

    let has_firecrawl_active = !firecrawl_connections.is_empty();
    let is_firecrawl_requested = provider_name.eq_ignore_ascii_case("firecrawl");

    if (is_firecrawl_requested || (has_firecrawl_active && provider_name.is_empty())) && has_firecrawl_active {
        let conn = &firecrawl_connections[0];
        let api_key = conn
            .data
            .api_key
            .as_deref()
            .or_else(|| conn.data.extra.get("apiKey").and_then(|v| v.as_str()))
            .or_else(|| conn.data.extra.get("token").and_then(|v| v.as_str()))
            .unwrap_or("");

        if !api_key.is_empty() {
            let base_url = conn.data.base_url.as_deref();
            match execute_firecrawl_scrape(
                state.proxy.client(),
                api_key,
                base_url,
                &req.url,
                req.format.as_deref(),
                req.max_characters,
            )
            .await
            {
                Ok(data) => {
                    return json_success(&WebFetchResponse {
                        success: true,
                        data,
                    });
                }
                Err((_, err_msg)) => {
                    tracing::warn!("Firecrawl scrape failed, attempting native fallback: {}", err_msg);
                }
            }
        }
    }

    // Native fallback with SSRF protection
    match execute_native_fetch(&req.url, req.format.as_deref(), req.max_characters).await {
        Ok(data) => json_success(&WebFetchResponse {
            success: true,
            data,
        }),
        Err((code, msg)) => json_error(code, &msg),
    }
}

/// Handler for POST /api/v1/search and POST /v1/search
pub async fn web_search_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(auth_resp) = authenticate_client(&headers, &state.db) {
        return auth_resp;
    }

    let req: WebSearchRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    if req.query.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing required field: query");
    }

    let limit = req.limit.unwrap_or(10).clamp(1, 50);
    let provider_name = req
        .provider
        .as_deref()
        .or(req.model.as_deref())
        .unwrap_or("");

    // Check Firecrawl provider
    let firecrawl_connections = state
        .db
        .get_active_provider_connections(Some("firecrawl"))
        .unwrap_or_default();

    let has_firecrawl_active = !firecrawl_connections.is_empty();
    let is_firecrawl_requested = provider_name.eq_ignore_ascii_case("firecrawl");

    if (is_firecrawl_requested || (has_firecrawl_active && provider_name.is_empty())) && has_firecrawl_active {
        let conn = &firecrawl_connections[0];
        let api_key = conn
            .data
            .api_key
            .as_deref()
            .or_else(|| conn.data.extra.get("apiKey").and_then(|v| v.as_str()))
            .or_else(|| conn.data.extra.get("token").and_then(|v| v.as_str()))
            .unwrap_or("");

        if !api_key.is_empty() {
            let base_url = conn.data.base_url.as_deref();
            match execute_firecrawl_search(state.proxy.client(), api_key, base_url, &req.query, limit).await {
                Ok(results) => {
                    return json_success(&WebSearchResponse { results });
                }
                Err((_, err_msg)) => {
                    tracing::warn!("Firecrawl search failed, falling back to DuckDuckGo: {}", err_msg);
                }
            }
        }
    }

    // Fallback: DuckDuckGo Instant API / HTML
    match execute_duckduckgo_search(state.proxy.client(), &req.query, limit).await {
        Ok(results) => json_success(&WebSearchResponse { results }),
        Err((code, msg)) => json_error(code, &msg),
    }
}

/// Handler for POST /v1/systemone and POST /api/v1/systemone
pub async fn systemone_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(auth_resp) = authenticate_client(&headers, &state.db) {
        return auth_resp;
    }

    let req: SystemOneRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    if req.model.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Missing model");
    }

    if req.state.is_null() {
        return json_error(StatusCode::BAD_REQUEST, "Missing required field: state");
    }

    // Check if an upstream connection exists for the model or its provider prefix
    let provider_candidate = req
        .model
        .split('/')
        .next()
        .unwrap_or(&req.model);

    let active_connections = state
        .db
        .get_active_provider_connections(Some(provider_candidate))
        .unwrap_or_default();

    if let Some(conn) = active_connections.first() {
        if let Some(base_url) = &conn.data.base_url {
            let upstream_url = format!("{}/v1/systemone", base_url.trim_end_matches('/'));
            let token = conn
                .data
                .api_key
                .as_deref()
                .or_else(|| conn.data.extra.get("accessToken").and_then(|v| v.as_str()));

            let mut upstream_req = state
                .proxy
                .client()
                .post(&upstream_url)
                .header(header::CONTENT_TYPE, "application/json")
                .timeout(Duration::from_secs(10));

            if let Some(tok) = token {
                upstream_req = upstream_req.header(header::AUTHORIZATION, format!("Bearer {}", tok));
            }

            if let Ok(resp) = upstream_req.json(&req).send().await {
                if resp.status().is_success() {
                    if let Ok(resp_json) = resp.json::<Value>().await {
                        // If upstream returned standard decision shape
                        if let (Some(decision), Some(conf)) = (
                            resp_json.get("decision").and_then(|v| v.as_str()),
                            resp_json.get("confidence").and_then(|v| v.as_f64()),
                        ) {
                            return json_success(&SystemOneResponse {
                                decision: decision.to_string(),
                                confidence: conf,
                                metadata: resp_json
                                    .get("metadata")
                                    .cloned()
                                    .unwrap_or_else(|| json!({})),
                            });
                        }

                        // If upstream returned answers.route format (e.g. Jev)
                        if let Some(route) = resp_json
                            .get("answers")
                            .and_then(|a| a.get("route").or_else(|| a.get("choice")))
                        {
                            if let Some(choice) = route.get("choice").and_then(|v| v.as_str()) {
                                let conf = route
                                    .get("confidence")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.95);
                                return json_success(&SystemOneResponse {
                                    decision: choice.to_string(),
                                    confidence: conf,
                                    metadata: json!({
                                        "model": req.model,
                                        "upstream_response": resp_json
                                    }),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Native decision evaluation
    let response = evaluate_system_one_decision(&req);
    json_success(&response)
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. SSRF Guard Tests ──

    #[test]
    fn test_ssrf_blocked_ipv4_ranges() {
        // Loopback
        assert!(is_blocked_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(127, 255, 255, 254)));

        // Private networks (RFC 1918)
        assert!(is_blocked_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(10, 254, 1, 10)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(192, 168, 254, 254)));

        // Link-local / Cloud metadata
        assert!(is_blocked_ipv4(Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(169, 254, 1, 1)));

        // CGNAT
        assert!(is_blocked_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(100, 127, 255, 255)));

        // Unspecified / Broadcast / Multicast
        assert!(is_blocked_ipv4(Ipv4Addr::new(0, 0, 0, 0)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(255, 255, 255, 255)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(224, 0, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(240, 0, 0, 1)));

        // Valid public IPs should NOT be blocked
        assert!(!is_blocked_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_blocked_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_blocked_ipv4(Ipv4Addr::new(93, 184, 216, 34))); // example.com
        assert!(!is_blocked_ipv4(Ipv4Addr::new(140, 82, 112, 4))); // github.com
    }

    #[test]
    fn test_ssrf_blocked_ipv6_ranges() {
        // Loopback and unspecified
        assert!(is_blocked_ipv6(Ipv6Addr::LOCALHOST));
        assert!(is_blocked_ipv6(Ipv6Addr::UNSPECIFIED));

        // Unique local (fc00::/7)
        let ula: Ipv6Addr = "fc00::1".parse().unwrap();
        assert!(is_blocked_ipv6(ula));
        let ula2: Ipv6Addr = "fd12:3456:789a::1".parse().unwrap();
        assert!(is_blocked_ipv6(ula2));

        // Link-local (fe80::/10)
        let link_local: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(is_blocked_ipv6(link_local));

        // Multicast (ff00::/8)
        let multi: Ipv6Addr = "ff02::1".parse().unwrap();
        assert!(is_blocked_ipv6(multi));

        // IPv4-mapped IPv6 pointing to private/loopback
        let mapped_loopback: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_blocked_ipv6(mapped_loopback));
        let mapped_private: Ipv6Addr = "::ffff:192.168.1.1".parse().unwrap();
        assert!(is_blocked_ipv6(mapped_private));

        // IPv4-mapped pointing to public IP should NOT be blocked
        let mapped_public: Ipv6Addr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(!is_blocked_ipv6(mapped_public));

        // 6to4 mapped private
        let six_to_four_priv: Ipv6Addr = "2002:7f00:0001::1".parse().unwrap(); // 127.0.0.1
        assert!(is_blocked_ipv6(six_to_four_priv));

        // Public IPv6 should NOT be blocked
        let public_v6: Ipv6Addr = "2606:4700:4700::1111".parse().unwrap(); // Cloudflare
        assert!(!is_blocked_ipv6(public_v6));
    }

    #[test]
    fn test_ssrf_blocked_hostnames_and_syntax() {
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("localhost."));
        assert!(is_blocked_hostname("sub.localhost"));
        assert!(is_blocked_hostname("host.local"));
        assert!(is_blocked_hostname("service.internal"));
        assert!(is_blocked_hostname("gateway.lan"));

        assert!(!is_blocked_hostname("example.com"));
        assert!(!is_blocked_hostname("api.firecrawl.dev"));
        assert!(!is_blocked_hostname("google.com"));

        // Syntax checks
        assert!(validate_url_syntax("http://127.0.0.1/status").is_err());
        assert!(validate_url_syntax("http://localhost:8080/metrics").is_err());
        assert!(validate_url_syntax("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_url_syntax("ftp://example.com/file").is_err());
        assert!(validate_url_syntax("file:///etc/passwd").is_err());

        // Valid syntax
        assert!(validate_url_syntax("https://example.com/blog").is_ok());
        assert!(validate_url_syntax("https://93.184.216.34/").is_ok());
    }

    // ── 2. Web Fetch Parsing & Markdown Conversion Tests ──

    #[test]
    fn test_html_title_extraction() {
        let html1 = "<html><head><title>My Awesome Page</title></head><body>Hello</body></html>";
        assert_eq!(extract_title(html1), Some("My Awesome Page".to_string()));

        let html2 = "<html><head><TITLE>  Spaces &amp; Entities &lt;Title&gt;  </TITLE></head></html>";
        assert_eq!(extract_title(html2), Some("Spaces & Entities <Title>".to_string()));

        let html3 = "<html><body><h1>Fallback Header Title</h1><p>Text</p></body></html>";
        assert_eq!(extract_title(html3), Some("Fallback Header Title".to_string()));

        let html_empty = "<html><body><p>No header or title</p></body></html>";
        assert_eq!(extract_title(html_empty), None);
    }

    #[test]
    fn test_html_to_markdown_conversion() {
        let sample_html = r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Test Page</title>
            <style>body { font-size: 14px; }</style>
            <script>console.log("secret script");</script>
        </head>
        <body>
            <!-- This is a comment -->
            <h1>Main Title</h1>
            <p>Welcome to <b>9Router</b>! It provides <i>high-performance</i> AI routing.</p>
            <p>Use <code>reqwest</code> for fetching content.</p>
            <ul>
                <li>Item 1</li>
                <li>Item 2</li>
            </ul>
            <p>Check our <a href="https://github.com/9router">GitHub repo</a> for details.</p>
        </body>
        </html>
        "#;

        let md = html_to_markdown(sample_html, "markdown", None);

        // Scripts, styles, and comments must be stripped
        assert!(!md.contains("font-size"));
        assert!(!md.contains("secret script"));
        assert!(!md.contains("This is a comment"));

        // Markdown elements should be present
        assert!(md.contains("# Main Title"));
        assert!(md.contains("**9Router**"));
        assert!(md.contains("*high-performance*"));
        assert!(md.contains("`reqwest`"));
        assert!(md.contains("* Item 1"));
        assert!(md.contains("* Item 2"));
        assert!(md.contains("GitHub repo"));
    }

    #[test]
    fn test_html_to_markdown_truncation() {
        let html = "<h1>Title</h1><p>0123456789ABCDEF</p>";
        let md = html_to_markdown(html, "markdown", Some(10));
        assert!(md.chars().count() <= 10);
    }

    #[test]
    fn test_web_fetch_request_deserialization() {
        let raw = r#"{
            "url": "https://example.com/article",
            "provider": "firecrawl",
            "format": "markdown",
            "max_characters": 5000
        }"#;

        let req: WebFetchRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.url, "https://example.com/article");
        assert_eq!(req.provider.as_deref(), Some("firecrawl"));
        assert_eq!(req.format.as_deref(), Some("markdown"));
        assert_eq!(req.max_characters, Some(5000));
    }

    // ── 3. Search Result Transformation Tests ──

    #[test]
    fn test_parse_firecrawl_search_response() {
        let sample_json = json!({
            "success": true,
            "data": [
                {
                    "title": "Rust Programming",
                    "url": "https://www.rust-lang.org/",
                    "description": "A language empowering everyone to build reliable and efficient software."
                },
                {
                    "title": "Rust by Example",
                    "url": "https://doc.rust-lang.org/rust-by-example/",
                    "description": "Learn Rust through annotated example programs."
                }
            ]
        });

        let results = parse_firecrawl_search_response(&sample_json, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].snippet, "A language empowering everyone to build reliable and efficient software.");
        assert_eq!(results[1].title, "Rust by Example");

        // Verify limit enforcement
        let limited = parse_firecrawl_search_response(&sample_json, 1);
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_parse_duckduckgo_instant_api() {
        let sample_json = json!({
            "Heading": "Rust (programming language)",
            "AbstractText": "Rust is a general-purpose programming language designed for performance and safety.",
            "AbstractURL": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "RelatedTopics": [
                {
                    "FirstURL": "https://duckduckgo.com/c/Compilers",
                    "Text": "Compilers - Software that translates computer code"
                },
                {
                    "FirstURL": "https://duckduckgo.com/c/System_software",
                    "Text": "System software - Computer software designed to provide a platform"
                }
            ]
        });

        let results = parse_duckduckgo_instant_api(&sample_json, 5);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Rust (programming language)");
        assert_eq!(results[0].url, "https://en.wikipedia.org/wiki/Rust_(programming_language)");
        assert_eq!(results[0].snippet, "Rust is a general-purpose programming language designed for performance and safety.");

        assert_eq!(results[1].title, "Compilers");
        assert_eq!(results[1].url, "https://duckduckgo.com/c/Compilers");
    }

    #[test]
    fn test_parse_duckduckgo_html() {
        let sample_html = r##"
        <div class="results">
            <div class="result results_links">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F">Rust Programming Language</a>
                </h2>
                <a class="result__snippet" href="#">A language empowering everyone to build reliable and efficient software.</a>
            </div>
            <div class="result results_links">
                <h2 class="result__title">
                    <a class="result__a" href="https://crates.io">Crates.io: Rust Package Registry</a>
                </h2>
                <a class="result__snippet" href="#">The official Rust package registry.</a>
            </div>
        </div>
        "##;

        let results = parse_duckduckgo_html(sample_html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].snippet, "A language empowering everyone to build reliable and efficient software.");
        assert_eq!(results[1].title, "Crates.io: Rust Package Registry");
        assert_eq!(results[1].url, "https://crates.io");
    }

    // ── 4. System One Decision Tests ──

    #[test]
    fn test_system_one_criteria_router() {
        let req = SystemOneRequest {
            model: "oc/jev-1.13-free".to_string(),
            state: json!("Please write a rust function to parse JSON with error handling: fn parse_data(s: &str) -> Result<Data, Error>"),
            questions: Some(json!({
                "route": {
                    "type": "choice",
                    "criteria": {
                        "claude-3-5-haiku": null,
                        "claude-3-5-sonnet": null,
                        "gpt-4o-mini": null
                    }
                }
            })),
            extra: HashMap::new(),
        };

        let res = evaluate_system_one_decision(&req);
        assert_eq!(res.decision, "claude-3-5-sonnet");
        assert!(res.confidence >= 0.95);
        assert_eq!(res.metadata["intent"], "coding");
    }

    #[test]
    fn test_system_one_math_reasoning() {
        let req = SystemOneRequest {
            model: "oc/jev-1.13-free".to_string(),
            state: json!("Solve the mathematical equation step by step: prove that the square root of 2 is irrational."),
            questions: Some(json!({
                "route": {
                    "criteria": {
                        "gpt-4o-mini": null,
                        "deepseek-r1": null,
                        "haiku": null
                    }
                }
            })),
            extra: HashMap::new(),
        };

        let res = evaluate_system_one_decision(&req);
        assert_eq!(res.decision, "deepseek-r1");
        assert!(res.confidence >= 0.94);
        assert_eq!(res.metadata["intent"], "reasoning");
    }

    #[test]
    fn test_system_one_heuristic_general() {
        let req = SystemOneRequest {
            model: "oc/jev-1.13-free".to_string(),
            state: json!("Hello! What is the capital of France?"),
            questions: None,
            extra: HashMap::new(),
        };

        let res = evaluate_system_one_decision(&req);
        assert_eq!(res.decision, "general");
        assert!(res.confidence > 0.8);
        assert_eq!(res.metadata["intent"], "chat_completion");
    }
}
