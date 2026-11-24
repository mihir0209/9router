use std::borrow::Cow;
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::config::RateLimitConfig;

/// Extracts client IP from request headers or socket peer address.
///
/// Evaluation precedence:
/// 1. `cf-connecting-ip` (Cloudflare edge)
/// 2. `x-forwarded-for` (first non-empty IP in proxy hop chain)
/// 3. `x-real-ip` (reverse proxy edge)
/// 4. `peer_addr` (direct TCP socket peer)
/// 5. Fallback: `"127.0.0.1"`
pub fn extract_client_ip(headers: &HeaderMap, peer_addr: Option<SocketAddr>) -> String {
    // 1. cf-connecting-ip
    if let Some(val) = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return normalize_ip(trimmed).into_owned();
        }
    }

    // 2. x-forwarded-for (first IP in chain)
    if let Some(val) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        for part in val.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                return normalize_ip(trimmed).into_owned();
            }
        }
    }

    // 3. x-real-ip
    if let Some(val) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return normalize_ip(trimmed).into_owned();
        }
    }

    // 4. socket peer address
    if let Some(addr) = peer_addr {
        return addr.ip().to_string();
    }

    // 5. Fallback
    "127.0.0.1".to_string()
}

/// Normalizes an IP string by stripping port numbers (if present) or parsing as IpAddr.
pub fn normalize_ip<'a>(raw: &'a str) -> Cow<'a, str> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Cow::Borrowed("127.0.0.1");
    }

    // Direct IpAddr parse (IPv4 e.g. 192.168.1.1 or IPv6 e.g. ::1)
    if let Ok(ip) = IpAddr::from_str(raw) {
        if ip.is_ipv4() {
            return Cow::Borrowed(raw);
        }
        return Cow::Owned(ip.to_string());
    }

    // SocketAddr parse (e.g. 192.168.1.1:8080 or [::1]:8080)
    if let Ok(sock) = SocketAddr::from_str(raw) {
        return Cow::Owned(sock.ip().to_string());
    }

    // IPv4 with port fallback (e.g. 192.168.1.1:8080 where SocketAddr failed)
    if let Some((ip_part, _)) = raw.split_once(':') {
        if let Ok(ip) = IpAddr::from_str(ip_part) {
            return Cow::Owned(ip.to_string());
        }
    }

    Cow::Borrowed(raw)
}

/// Structured error returned when an IP or Model exceeds configured sliding window quotas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateLimitError {
    IpRateLimited {
        ip: String,
        limit: u32,
        window_secs: u64,
        retry_after: u64,
        quota_type: &'static str, // "RPM" or "RPS"
    },
    ModelRateLimited {
        model: String,
        limit: u64,
        window_secs: u64,
        retry_after: u64,
        quota_type: &'static str, // "RPM" or "TPM"
    },
}

impl RateLimitError {
    pub fn retry_after(&self) -> u64 {
        match self {
            Self::IpRateLimited { retry_after, .. } => *retry_after,
            Self::ModelRateLimited { retry_after, .. } => *retry_after,
        }
    }

    pub fn to_response(&self) -> Response {
        let (message, param, retry_after) = match self {
            Self::IpRateLimited {
                ip,
                limit,
                window_secs,
                retry_after,
                quota_type,
            } => {
                let msg = format!(
                    "Rate limit exceeded for client IP {}: limit of {} requests per {}s ({}) exceeded. Please retry after {}s.",
                    ip, limit, window_secs, quota_type, retry_after
                );
                (msg, "ip", *retry_after)
            }
            Self::ModelRateLimited {
                model,
                limit,
                window_secs,
                retry_after,
                quota_type,
            } => {
                let unit = if *quota_type == "TPM" { "tokens" } else { "requests" };
                let msg = format!(
                    "Rate limit exceeded for model '{}': limit of {} {} per {}s ({}) exceeded. Please retry after {}s.",
                    model, limit, unit, window_secs, quota_type, retry_after
                );
                (msg, "model", *retry_after)
            }
        };

        let body = Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "rate_limit_error",
                "param": param,
                "code": StatusCode::TOO_MANY_REQUESTS.as_u16(),
            }
        }));

        let mut res = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
        let retry_val = retry_after.max(1).to_string();
        if let Ok(hdr) = retry_val.parse() {
            res.headers_mut().insert(header::RETRY_AFTER, hdr);
        }
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        res
    }
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IpRateLimited { ip, limit, window_secs, quota_type, retry_after } => {
                write!(
                    f,
                    "Client IP {} exceeded {} limit ({} requests/{}s, retry after {}s)",
                    ip, quota_type, limit, window_secs, retry_after
                )
            }
            Self::ModelRateLimited { model, limit, window_secs, quota_type, retry_after } => {
                write!(
                    f,
                    "Model '{}' exceeded {} limit ({}/{}s, retry after {}s)",
                    model, quota_type, limit, window_secs, retry_after
                )
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

impl IntoResponse for RateLimitError {
    fn into_response(self) -> Response {
        self.to_response()
    }
}

/// Sliding window state tracked for a single client IP using ring-buffered timestamps.
#[derive(Debug, Clone)]
pub struct IpWindowState {
    pub timestamps: VecDeque<Instant>,
    pub last_access: Instant,
}

impl Default for IpWindowState {
    fn default() -> Self {
        Self {
            timestamps: VecDeque::with_capacity(32),
            last_access: Instant::now(),
        }
    }
}

/// Sliding window state tracked for a single model (RPM + TPM) using ring buffers.
#[derive(Debug, Clone)]
pub struct ModelWindowState {
    pub request_timestamps: VecDeque<Instant>,
    pub token_entries: VecDeque<(Instant, u64)>,
    pub total_tokens: u64,
    pub last_access: Instant,
}

impl Default for ModelWindowState {
    fn default() -> Self {
        Self {
            request_timestamps: VecDeque::with_capacity(32),
            token_entries: VecDeque::with_capacity(32),
            total_tokens: 0,
            last_access: Instant::now(),
        }
    }
}

/// In-memory sliding window rate limiter for per-client-IP and per-model quotas.
///
/// Thread-safe and lock-free across distinct keys using sharded DashMaps.
/// Sub-microsecond overhead (<100ns typical latency) under high multi-threaded concurrency.
pub struct RateLimiter {
    pub config: RwLock<RateLimitConfig>,
    ip_windows: DashMap<String, IpWindowState>,
    model_windows: DashMap<String, ModelWindowState>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

impl RateLimiter {
    /// Creates a new RateLimiter with the specified configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config: RwLock::new(config),
            ip_windows: DashMap::new(),
            model_windows: DashMap::new(),
        }
    }

    /// Initializes a RateLimiter from root application Config.
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::new(config.rate_limit.clone())
    }

    /// Updates the running configuration in-place.
    pub fn update_config(&self, new_config: RateLimitConfig) {
        let mut cfg = self.config.write().unwrap();
        *cfg = new_config;
    }

    /// Merges dynamic settings JSON from the database into the rate limiter configuration.
    pub fn reload_from_settings(&self, settings: &serde_json::Value) {
        let mut cfg = self.config.write().unwrap();
        cfg.merge_from_settings(settings);
    }

    /// Evaluates per-client-IP sliding window limits.
    ///
    /// Checks RPS (1-second sliding window) and RPM (configurable window, default 60s).
    /// If within limits, registers the request and returns `Ok(())`.
    /// If limits are exceeded, returns `Err(RateLimitError::IpRateLimited)`.
    pub fn check_ip(&self, ip: &str) -> Result<(), RateLimitError> {
        let cfg = self.config.read().unwrap();
        if !cfg.ip_rate_limit_enabled || (cfg.ip_rpm.is_none() && cfg.ip_rps.is_none()) {
            return Ok(());
        }

        let trimmed_ip = ip.trim();

        // Check allowlist
        if !cfg.ip_allowlist.is_empty() {
            let norm_ip = normalize_ip(trimmed_ip);
            let norm_ref = norm_ip.as_ref();
            if cfg.ip_allowlist.iter().any(|allowed| {
                let allowed_norm = normalize_ip(allowed);
                allowed_norm.as_ref() == norm_ref
                    || (allowed == "localhost" && (norm_ref == "127.0.0.1" || norm_ref == "::1"))
            }) {
                return Ok(());
            }
        }

        let now = Instant::now();
        let window_secs = cfg.ip_window_secs.max(1);
        let rpm_window = Duration::from_secs(window_secs);
        let rps_window = Duration::from_secs(1);
        let max_window = rpm_window.max(rps_window);

        // Ultra-fast path: direct match on trimmed IP in ip_windows without parsing
        if let Some(mut entry) = self.ip_windows.get_mut(trimmed_ip) {
            let state = entry.value_mut();
            state.last_access = now;

            // O(1) Amortized ring-buffer expiration: prune only elements older than max_window from front
            while let Some(&front) = state.timestamps.front() {
                if now.saturating_duration_since(front) >= max_window {
                    state.timestamps.pop_front();
                } else {
                    break;
                }
            }

            // 1. Check RPS limit (last 1 second) via binary search partition
            if let Some(rps) = cfg.ip_rps {
                let rps_cutoff = now.checked_sub(rps_window).unwrap_or(now);
                let rps_start = state.timestamps.partition_point(|&t| t < rps_cutoff);
                let rps_count = state.timestamps.len() - rps_start;
                if rps_count >= rps as usize {
                    let oldest = state.timestamps[rps_start];
                    let elapsed = now.saturating_duration_since(oldest);
                    let retry_after = rps_window.saturating_sub(elapsed).as_secs().max(1);
                    return Err(RateLimitError::IpRateLimited {
                        ip: trimmed_ip.to_string(),
                        limit: rps,
                        window_secs: 1,
                        retry_after,
                        quota_type: "RPS",
                    });
                }
            }

            // 2. Check RPM limit (configured window duration, default 60s)
            if let Some(rpm) = cfg.ip_rpm {
                let rpm_cutoff = now.checked_sub(rpm_window).unwrap_or(now);
                let rpm_start = state.timestamps.partition_point(|&t| t < rpm_cutoff);
                let rpm_count = state.timestamps.len() - rpm_start;
                if rpm_count >= rpm as usize {
                    let oldest = state.timestamps[rpm_start];
                    let elapsed = now.saturating_duration_since(oldest);
                    let retry_after = rpm_window.saturating_sub(elapsed).as_secs().max(1);
                    return Err(RateLimitError::IpRateLimited {
                        ip: trimmed_ip.to_string(),
                        limit: rpm,
                        window_secs,
                        retry_after,
                        quota_type: "RPM",
                    });
                }
            }

            // Request permitted
            state.timestamps.push_back(now);
            return Ok(());
        }

        // Normalized match
        let norm_ip = normalize_ip(trimmed_ip);
        if norm_ip.as_ref() != trimmed_ip {
            if let Some(mut entry) = self.ip_windows.get_mut(norm_ip.as_ref()) {
                let state = entry.value_mut();
                state.last_access = now;

                while let Some(&front) = state.timestamps.front() {
                    if now.saturating_duration_since(front) >= max_window {
                        state.timestamps.pop_front();
                    } else {
                        break;
                    }
                }

                if let Some(rps) = cfg.ip_rps {
                    let rps_cutoff = now.checked_sub(rps_window).unwrap_or(now);
                    let rps_start = state.timestamps.partition_point(|&t| t < rps_cutoff);
                    let rps_count = state.timestamps.len() - rps_start;
                    if rps_count >= rps as usize {
                        let oldest = state.timestamps[rps_start];
                        let elapsed = now.saturating_duration_since(oldest);
                        let retry_after = rps_window.saturating_sub(elapsed).as_secs().max(1);
                        return Err(RateLimitError::IpRateLimited {
                            ip: norm_ip.into_owned(),
                            limit: rps,
                            window_secs: 1,
                            retry_after,
                            quota_type: "RPS",
                        });
                    }
                }

                if let Some(rpm) = cfg.ip_rpm {
                    let rpm_cutoff = now.checked_sub(rpm_window).unwrap_or(now);
                    let rpm_start = state.timestamps.partition_point(|&t| t < rpm_cutoff);
                    let rpm_count = state.timestamps.len() - rpm_start;
                    if rpm_count >= rpm as usize {
                        let oldest = state.timestamps[rpm_start];
                        let elapsed = now.saturating_duration_since(oldest);
                        let retry_after = rpm_window.saturating_sub(elapsed).as_secs().max(1);
                        return Err(RateLimitError::IpRateLimited {
                            ip: norm_ip.into_owned(),
                            limit: rpm,
                            window_secs,
                            retry_after,
                            quota_type: "RPM",
                        });
                    }
                }

                state.timestamps.push_back(now);
                return Ok(());
            }
        }

        // Slow path: new entry
        let mut entry = self.ip_windows.entry(norm_ip.into_owned()).or_default();
        let state = entry.value_mut();
        state.last_access = now;
        state.timestamps.push_back(now);
        Ok(())
    }

    /// Evaluates IP rate limit using incoming request headers and returns an HTTP 429 Response on rejection.
    pub fn check_ip_headers(&self, headers: &HeaderMap) -> Result<(), Response> {
        let ip = extract_client_ip(headers, None);
        self.check_ip(&ip).map_err(|e| e.to_response())
    }

    /// Evaluates per-model sliding window limits for requests (RPM) and tokens (TPM).
    ///
    /// `estimated_tokens` represents the pre-flight prompt token estimate.
    /// If within limits, increments RPM counter and returns `Ok(())`.
    pub fn check_model(&self, raw_model: &str, estimated_tokens: u64) -> Result<(), RateLimitError> {
        let cfg = self.config.read().unwrap();
        if !cfg.model_rate_limit_enabled {
            return Ok(());
        }

        // Normalize model identifier (e.g., "openai/gpt-4o" -> "gpt-4o")
        let clean_model = if let Some((_, stripped)) = raw_model.split_once('/') {
            stripped
        } else {
            raw_model
        };

        // Resolve model limits: exact raw -> stripped clean -> default model quotas
        let model_limits = cfg
            .model_limits
            .get(raw_model)
            .or_else(|| cfg.model_limits.get(clean_model));

        let effective_rpm = model_limits
            .and_then(|l| l.rpm)
            .or(cfg.model_default_rpm);
        let effective_tpm = model_limits
            .and_then(|l| l.tpm)
            .or(cfg.model_default_tpm);

        if effective_rpm.is_none() && effective_tpm.is_none() {
            return Ok(());
        }

        let now = Instant::now();
        let window_secs = 60u64;
        let window_dur = Duration::from_secs(window_secs);

        // Fast path: existing entry
        if let Some(mut entry) = self.model_windows.get_mut(clean_model) {
            let state = entry.value_mut();
            state.last_access = now;

            // O(1) Amortized ring-buffer expiration for model timestamps and tokens
            while let Some(&front) = state.request_timestamps.front() {
                if now.saturating_duration_since(front) >= window_dur {
                    state.request_timestamps.pop_front();
                } else {
                    break;
                }
            }
            while let Some(&(front_time, tok)) = state.token_entries.front() {
                if now.saturating_duration_since(front_time) >= window_dur {
                    state.total_tokens = state.total_tokens.saturating_sub(tok);
                    state.token_entries.pop_front();
                } else {
                    break;
                }
            }

            // 1. Check RPM
            if let Some(rpm) = effective_rpm {
                if state.request_timestamps.len() >= rpm as usize {
                    let oldest = state.request_timestamps.front().copied().unwrap_or(now);
                    let elapsed = now.saturating_duration_since(oldest);
                    let retry_after = window_dur.saturating_sub(elapsed).as_secs().max(1);
                    return Err(RateLimitError::ModelRateLimited {
                        model: clean_model.to_string(),
                        limit: rpm as u64,
                        window_secs,
                        retry_after,
                        quota_type: "RPM",
                    });
                }
            }

            // 2. Check TPM
            if let Some(tpm) = effective_tpm {
                if state.total_tokens + estimated_tokens > tpm || (state.total_tokens >= tpm && estimated_tokens == 0) {
                    let mut accumulated = state.total_tokens;
                    let mut retry_after = window_secs;
                    for (t, tok) in &state.token_entries {
                        accumulated = accumulated.saturating_sub(*tok);
                        if accumulated + estimated_tokens <= tpm {
                            let elapsed = now.saturating_duration_since(*t);
                            retry_after = window_dur.saturating_sub(elapsed).as_secs().max(1);
                            break;
                        }
                    }

                    return Err(RateLimitError::ModelRateLimited {
                        model: clean_model.to_string(),
                        limit: tpm,
                        window_secs,
                        retry_after,
                        quota_type: "TPM",
                    });
                }
            }

            // Request allowed: record RPM request timestamp and initial token reservation
            state.request_timestamps.push_back(now);
            if estimated_tokens > 0 {
                state.total_tokens = state.total_tokens.saturating_add(estimated_tokens);
                state.token_entries.push_back((now, estimated_tokens));
            }

            return Ok(());
        }

        // Slow path: new entry
        let mut entry = self.model_windows.entry(clean_model.to_string()).or_default();
        let state = entry.value_mut();
        state.last_access = now;
        state.request_timestamps.push_back(now);
        if estimated_tokens > 0 {
            state.total_tokens = state.total_tokens.saturating_add(estimated_tokens);
            state.token_entries.push_back((now, estimated_tokens));
        }

        Ok(())
    }

    /// Evaluates model rate limit and returns an HTTP 429 Response on rejection.
    pub fn check_model_rate_limit(&self, model: &str, estimated_tokens: u64) -> Result<(), Response> {
        self.check_model(model, estimated_tokens).map_err(|e| e.to_response())
    }

    /// Records completed token consumption (prompt + completion) for TPM accounting.
    pub fn record_model_tokens(&self, raw_model: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }

        let clean_model = if let Some((_, stripped)) = raw_model.split_once('/') {
            stripped
        } else {
            raw_model
        };

        let now = Instant::now();
        if let Some(mut entry) = self.model_windows.get_mut(clean_model) {
            let state = entry.value_mut();
            state.last_access = now;
            state.total_tokens = state.total_tokens.saturating_add(tokens);
            state.token_entries.push_back((now, tokens));
        } else {
            let mut entry = self.model_windows.entry(clean_model.to_string()).or_default();
            let state = entry.value_mut();
            state.last_access = now;
            state.total_tokens = state.total_tokens.saturating_add(tokens);
            state.token_entries.push_back((now, tokens));
        }
    }

    /// Explicitly resets the sliding window for a specific client IP.
    pub fn reset_ip(&self, ip: &str) {
        let norm_ip = normalize_ip(ip);
        self.ip_windows.remove(norm_ip.as_ref());
    }

    /// Explicitly resets the sliding window for a specific model.
    pub fn reset_model(&self, model: &str) {
        let clean = if let Some((_, s)) = model.split_once('/') { s } else { model };
        self.model_windows.remove(clean);
        self.model_windows.remove(model);
    }

    /// Explicitly resets all client IP and model sliding windows.
    pub fn reset_all(&self) {
        self.ip_windows.clear();
        self.model_windows.clear();
    }

    /// Active expiration and cleanup routine for idle client IP and model entries.
    ///
    /// Entries without activity for longer than `max_idle` duration are pruned from memory.
    /// Returns `(pruned_ips, pruned_models)`.
    pub fn cleanup(&self, max_idle: Duration) -> (usize, usize) {
        let mut pruned_ips = 0;
        let mut pruned_models = 0;

        self.ip_windows.retain(|_, state| {
            if state.last_access.elapsed() >= max_idle {
                pruned_ips += 1;
                false
            } else {
                true
            }
        });

        self.model_windows.retain(|_, state| {
            if state.last_access.elapsed() >= max_idle {
                pruned_models += 1;
                false
            } else {
                true
            }
        });

        (pruned_ips, pruned_models)
    }

    /// Spawns an asynchronous background task to periodically clean up expired rate limiter entries.
    pub fn spawn_cleanup_task(
        self: Arc<Self>,
        interval: Duration,
        max_idle: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let (pruned_ips, pruned_models) = self.cleanup(max_idle);
                if pruned_ips > 0 || pruned_models > 0 {
                    tracing::debug!(
                        "[RateLimiter] Auto-cleanup pruned {} idle IP(s) and {} idle model(s)",
                        pruned_ips,
                        pruned_models
                    );
                }
            }
        })
    }

    /// Returns the current number of tracked IP windows.
    pub fn tracked_ips_count(&self) -> usize {
        self.ip_windows.len()
    }

    /// Returns the current number of tracked model windows.
    pub fn tracked_models_count(&self) -> usize {
        self.model_windows.len()
    }
}
