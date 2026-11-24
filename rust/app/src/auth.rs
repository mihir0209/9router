use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::db::{ApiKey, Database, KeyValidationError};
pub use crate::ratelimit::*;

/// Standard auth error representation aligning with OpenAI error responses.
#[derive(Debug, Clone)]
pub enum AuthError {
    MissingApiKey,
    InvalidApiKey,
    DisabledApiKey,
    ExpiredApiKey,
    RateLimitExceeded {
        limit: i64,
        window_seconds: i64,
    },
    IpRateLimitExceeded {
        ip: String,
        limit: u32,
        window_seconds: u64,
        retry_after: u64,
        quota_type: &'static str,
    },
    ModelRateLimitExceeded {
        model: String,
        limit: u64,
        quota_type: &'static str,
        window_seconds: u64,
        retry_after: u64,
    },
    ModelForbidden(String),
    Internal(String),
}

impl From<RateLimitError> for AuthError {
    fn from(err: RateLimitError) -> Self {
        match err {
            RateLimitError::IpRateLimited {
                ip,
                limit,
                window_secs,
                retry_after,
                quota_type,
            } => Self::IpRateLimitExceeded {
                ip,
                limit,
                window_seconds: window_secs,
                retry_after,
                quota_type,
            },
            RateLimitError::ModelRateLimited {
                model,
                limit,
                window_secs,
                retry_after,
                quota_type,
            } => Self::ModelRateLimitExceeded {
                model,
                limit,
                quota_type,
                window_seconds: window_secs,
                retry_after,
            },
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let mut retry_after_hdr: Option<u64> = None;
        let (status, message, error_type, param): (StatusCode, String, &'static str, Option<&'static str>) = match self {
            Self::MissingApiKey => (
                StatusCode::UNAUTHORIZED,
                "Missing API key. Provide via 'Authorization: Bearer <key>' or 'x-api-key' header.".to_string(),
                "authentication_error",
                None,
            ),
            Self::InvalidApiKey => (
                StatusCode::UNAUTHORIZED,
                "Invalid API key provided.".to_string(),
                "authentication_error",
                None,
            ),
            Self::DisabledApiKey => (
                StatusCode::UNAUTHORIZED,
                "The provided API key is disabled.".to_string(),
                "authentication_error",
                None,
            ),
            Self::ExpiredApiKey => (
                StatusCode::UNAUTHORIZED,
                "The provided API key has expired.".to_string(),
                "authentication_error",
                None,
            ),
            Self::RateLimitExceeded {
                limit,
                window_seconds,
            } => {
                retry_after_hdr = Some(1);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Rate limit exceeded: max {} requests per {}s.",
                        limit, window_seconds
                    ),
                    "rate_limit_error",
                    None,
                )
            }
            Self::IpRateLimitExceeded {
                ip,
                limit,
                window_seconds,
                retry_after,
                quota_type,
            } => {
                retry_after_hdr = Some(retry_after.max(1));
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Rate limit exceeded for client IP {}: limit of {} requests per {}s ({}) exceeded. Please retry after {}s.",
                        ip, limit, window_seconds, quota_type, retry_after
                    ),
                    "rate_limit_error",
                    Some("ip"),
                )
            }
            Self::ModelRateLimitExceeded {
                model,
                limit,
                quota_type,
                window_seconds,
                retry_after,
            } => {
                retry_after_hdr = Some(retry_after.max(1));
                let unit = if quota_type == "TPM" { "tokens" } else { "requests" };
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Rate limit exceeded for model '{}': limit of {} {} per {}s ({}) exceeded. Please retry after {}s.",
                        model, limit, unit, window_seconds, quota_type, retry_after
                    ),
                    "rate_limit_error",
                    Some("model"),
                )
            }
            Self::ModelForbidden(reason) => {
                (StatusCode::FORBIDDEN, reason, "permission_denied", None)
            }
            Self::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                msg,
                "internal_server_error",
                None,
            ),
        };

        let body = Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": param,
                "code": status.as_u16(),
            }
        }));

        let mut res = (status, body).into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_str = retry_after_hdr.unwrap_or(1).to_string();
            if let Ok(hdr) = retry_str.parse() {
                res.headers_mut().insert(header::RETRY_AFTER, hdr);
            }
        }
        res
    }
}

/// Extracted and validated API key from incoming HTTP request.
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    pub key: ApiKey,
    pub raw_key: String,
}

impl AuthenticatedKey {
    /// Validates whether the key is authorized to access the requested model.
    pub fn enforce_model(&self, db: &Database, model: &str) -> Result<(), AuthError> {
        db.check_model_allowed(&self.key, model)
            .map_err(|err| match err {
                KeyValidationError::ModelBlocked(m) => {
                    AuthError::ModelForbidden(format!("Model '{}' is blocked for this key", m))
                }
                KeyValidationError::ModelNotAllowed(m) => {
                    AuthError::ModelForbidden(format!("Model '{}' is not in allowlist", m))
                }
                _ => AuthError::InvalidApiKey,
            })
    }
}

/// Helper function to parse API key from headers.
/// Checks Authorization: Bearer <key>, x-api-key, or api-key.
pub fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // 1. Check Authorization header
    if let Some(auth_val) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            let trimmed = auth_str.trim();
            if let Some(token) = trimmed.strip_prefix("Bearer ") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            } else if let Some(token) = trimmed.strip_prefix("bearer ") {
                let token = token.trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }

    // 2. Check x-api-key header
    if let Some(val) = headers.get("x-api-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 3. Check api-key header
    if let Some(val) = headers.get("api-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 4. Check x-goog-api-key header (Gemini SDKs)
    if let Some(val) = headers.get("x-goog-api-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 5. Check x-router-key header
    if let Some(val) = headers.get("x-router-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 5. Check x-admin-key header
    if let Some(val) = headers.get("x-admin-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

/// Helper to decode base64url data without padding (RFC 7515).
fn decode_base64_url(input: &str) -> Result<Vec<u8>, ()> {
    use base64::Engine;
    let trimmed = input.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(input))
        .map_err(|_| ())
}

/// Extracts session token from Cookie header (auth_token=...) or Authorization header.
pub fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(cookie_val) = headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_val.to_str() {
            for part in cookie_str.split(';') {
                let part = part.trim();
                if let Some(token) = part.strip_prefix("auth_token=") {
                    let token = token.trim();
                    if !token.is_empty() {
                        return Some(token.to_string());
                    }
                }
            }
        }
    }

    if let Some(auth_val) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            let trimmed = auth_str.trim();
            if let Some(token) = trimmed.strip_prefix("Bearer ").or_else(|| trimmed.strip_prefix("bearer ")) {
                let token = token.trim();
                if token.split('.').count() == 3 {
                    return Some(token.to_string());
                }
            }
        }
    }

    None
}

/// Verifies HS256 JWT signature and claims generated by Next.js / jose SignJWT.
pub fn verify_dashboard_jwt(token: &str, jwt_secret: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return false;
    }

    let Ok(sig_bytes) = decode_base64_url(parts[2]) else {
        return false;
    };

    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, jwt_secret.as_bytes());
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    if ring::hmac::verify(&key, signing_input.as_bytes(), &sig_bytes).is_err() {
        return false;
    }

    let Ok(payload_bytes) = decode_base64_url(parts[1]) else {
        return false;
    };

    let Ok(payload): Result<serde_json::Value, _> = serde_json::from_slice(&payload_bytes) else {
        return false;
    };

    if let Some(exp) = payload.get("exp").and_then(|v| v.as_i64()) {
        let now = chrono::Utc::now().timestamp();
        if now > exp {
            return false;
        }
    }

    if payload.get("authenticated").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }

    false
}

/// Generates an HS256 JWT compatible with Next.js dashboard auth verification.
pub fn create_dashboard_jwt(jwt_secret: &str) -> String {
    use base64::Engine;
    let now = chrono::Utc::now().timestamp();
    let exp = now + 86400; // 24 hours
    let header_json = r#"{"alg":"HS256","typ":"JWT"}"#;
    let payload_json = serde_json::json!({
        "authenticated": true,
        "iat": now,
        "exp": exp
    })
    .to_string();

    let header_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let signing_input = format!("{}.{}", header_b64, payload_b64);

    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, jwt_secret.as_bytes());
    let tag = ring::hmac::sign(&key, signing_input.as_bytes());
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref());

    format!("{}.{}", signing_input, sig_b64)
}

/// Generate API key with machineId embedded in sk-{machineId}-{keyId}-{crc8} format.
pub fn generate_api_key(machine_id: Option<&str>) -> String {
    use ring::rand::SecureRandom;
    let machine = machine_id.unwrap_or("0000000000000000");
    let machine_slice = if machine.len() >= 16 { &machine[..16] } else { machine };
    let rng = ring::rand::SystemRandom::new();
    let mut random_bytes = [0u8; 6];
    let _ = rng.fill(&mut random_bytes);
    let chars = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let key_id: String = random_bytes.iter().map(|b| chars[(*b as usize) % chars.len()] as char).collect();

    let secret = std::env::var("API_KEY_SECRET").unwrap_or_else(|_| "endpoint-proxy-api-key-secret".to_string());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let mut ctx = ring::hmac::Context::with_key(&key);
    ctx.update(format!("{}{}", machine_slice, key_id).as_bytes());
    let tag = ctx.sign();
    let hex_sig: String = tag.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    let crc8 = &hex_sig[..8.min(hex_sig.len())];
    format!("sk-{}-{}-{}", machine_slice, key_id, crc8)
}

/// Constant-time comparison of two secret byte slices to prevent timing attacks.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Resolves the configured Admin API token from environment variables or .env file.
/// Supports ADMIN_API_TOKEN, ADMIN_KEY, ROUTER_API_KEY, NINEROUTER_API_KEY, and NINE_ROUTER_API_KEY.
pub fn resolve_configured_admin_token() -> String {
    for var in &[
        "ADMIN_API_TOKEN",
        "ADMIN_KEY",
        "ROUTER_API_KEY",
        "NINEROUTER_API_KEY",
        "NINE_ROUTER_API_KEY",
    ] {
        if let Ok(tok) = std::env::var(var) {
            let trimmed = tok.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }

    let candidates = [
        std::path::PathBuf::from(".env"),
        std::path::PathBuf::from("/devspace/Projects/9router/.env"),
        crate::config::resolve_data_dir().join(".env"),
    ];
    for p in &candidates {
        if let Ok(content) = std::fs::read_to_string(p) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once('=') {
                    let k = k.trim();
                    if k == "ADMIN_API_TOKEN"
                        || k == "ADMIN_KEY"
                        || k == "ROUTER_API_KEY"
                        || k == "NINEROUTER_API_KEY"
                        || k == "NINE_ROUTER_API_KEY"
                    {
                        let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                        if !val.is_empty() {
                            return val;
                        }
                    }
                }
            }
        }
    }
    String::new()
}

/// Validates Admin API token passed via x-admin-key or Authorization: Bearer.
/// Allows automated internal systems, CI/CD, and scripts to manage keys and providers.
pub fn verify_admin_api_token(headers: &HeaderMap) -> bool {
    let mut configured_tokens = Vec::new();
    let admin_tok = resolve_configured_admin_token();
    if !admin_tok.trim().is_empty() {
        configured_tokens.push(admin_tok);
    }
    if let Ok(internal_tok) = std::env::var("INTERNAL_API_TOKEN") {
        let trimmed = internal_tok.trim().to_string();
        if !trimmed.is_empty() && !configured_tokens.contains(&trimmed) {
            configured_tokens.push(trimmed);
        }
    }
    for var in &[
        "ROUTER_API_KEY",
        "NINEROUTER_API_KEY",
        "NINE_ROUTER_API_KEY",
        "ADMIN_API_TOKEN",
        "ADMIN_KEY",
    ] {
        if let Ok(tok) = std::env::var(var) {
            let trimmed = tok.trim().to_string();
            if !trimmed.is_empty() && !configured_tokens.contains(&trimmed) {
                configured_tokens.push(trimmed);
            }
        }
    }

    if configured_tokens.is_empty() {
        return false;
    }

    // 1. Check x-admin-key, x-internal-token, x-router-key, x-9r-cli-token, x-api-key headers
    for header_name in &[
        "x-admin-key",
        "x-internal-token",
        "x-router-key",
        "x-9r-cli-token",
        "x-api-key",
    ] {
        if let Some(val) = headers.get(*header_name) {
            if let Ok(s) = val.to_str() {
                let s = s.trim();
                for tok in &configured_tokens {
                    if constant_time_eq(s.as_bytes(), tok.as_bytes()) {
                        return true;
                    }
                }
            }
        }
    }

    // 2. Check Authorization: Bearer <ADMIN_API_TOKEN>
    if let Some(auth_val) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            let trimmed = auth_str.trim();
            if let Some(token) = trimmed.strip_prefix("Bearer ").or_else(|| trimmed.strip_prefix("bearer ")) {
                let token = token.trim();
                for tok in &configured_tokens {
                    if constant_time_eq(token.as_bytes(), tok.as_bytes()) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Comprehensive internal API guard for dashboard management routes:
/// 1. Allowed if requireLogin is false in settings
/// 2. Allowed if request presents a valid Admin API token (for automation)
/// 3. Allowed if request presents a valid logged-in dashboard session cookie (for users)
pub fn verify_internal_auth(
    headers: &HeaderMap,
    jwt_secret: &str,
    db: &Database,
) -> Result<(), Response> {
    // Check if requireLogin is disabled (default is false when not configured or no password set)
    if let Ok(settings_map) = db.load_settings() {
        if !settings_map.get("requireLogin").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Ok(());
        }
    } else {
        return Ok(());
    }

    // Check Admin API token for automation / programmatic scripts
    if verify_admin_api_token(headers) {
        return Ok(());
    }

    // Check dashboard session cookie / JWT for browser users
    if let Some(session_token) = extract_session_token(headers) {
        if verify_dashboard_jwt(&session_token, jwt_secret) {
            return Ok(());
        }
    }

    Err((
        StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(json!({
            "error": "Unauthorized: Valid dashboard session cookie or Admin API key required",
            "code": 401
        })),
    )
        .into_response())
}

/// Axum extractor for mandatory API key authentication.
impl<S> FromRequestParts<S> for AuthenticatedKey
where
    S: Send + Sync,
    Arc<Database>: FromRef<S>,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let raw_key = extract_api_key(&parts.headers).ok_or(AuthError::MissingApiKey)?;
        let db = Arc::<Database>::from_ref(state);

        let api_key = db
            .get_api_key_by_key(&raw_key)
            .map_err(|e| AuthError::Internal(e.to_string()))?
            .ok_or(AuthError::InvalidApiKey)?;

        db.validate_key(&api_key).map_err(|err| match err {
            KeyValidationError::Disabled => AuthError::DisabledApiKey,
            KeyValidationError::Expired => AuthError::ExpiredApiKey,
            KeyValidationError::RateLimited {
                limit,
                window_seconds,
            } => AuthError::RateLimitExceeded {
                limit,
                window_seconds,
            },
            _ => AuthError::InvalidApiKey,
        })?;

        Ok(AuthenticatedKey {
            key: api_key,
            raw_key,
        })
    }
}

/// Optional API key extractor (used for endpoints where authentication is optional).
#[derive(Debug, Clone)]
pub struct OptionalAuthenticatedKey(pub Option<AuthenticatedKey>);

impl<S> FromRequestParts<S> for OptionalAuthenticatedKey
where
    S: Send + Sync,
    Arc<Database>: FromRef<S>,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(raw_key) = extract_api_key(&parts.headers) {
            let db = Arc::<Database>::from_ref(state);
            let api_key = db
                .get_api_key_by_key(&raw_key)
                .map_err(|e| AuthError::Internal(e.to_string()))?
                .ok_or(AuthError::InvalidApiKey)?;

            db.validate_key(&api_key).map_err(|err| match err {
                KeyValidationError::Disabled => AuthError::DisabledApiKey,
                KeyValidationError::Expired => AuthError::ExpiredApiKey,
                KeyValidationError::RateLimited {
                    limit,
                    window_seconds,
                } => AuthError::RateLimitExceeded {
                    limit,
                    window_seconds,
                },
                _ => AuthError::InvalidApiKey,
            })?;

            Ok(OptionalAuthenticatedKey(Some(AuthenticatedKey {
                key: api_key,
                raw_key,
            })))
        } else {
            Ok(OptionalAuthenticatedKey(None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RateLimitConfig;
    use axum::http::Request;
    use chrono::Utc;
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_extract_api_key_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer sk-test-bearer-token".parse().unwrap(),
        );
        assert_eq!(
            extract_api_key(&headers),
            Some("sk-test-bearer-token".to_string())
        );
    }

    #[test]
    fn test_extract_api_key_x_api_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-custom-x-api-key".parse().unwrap());
        assert_eq!(
            extract_api_key(&headers),
            Some("sk-custom-x-api-key".to_string())
        );
    }

    #[test]
    fn test_extract_api_key_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_api_key(&headers), None);
    }

    #[tokio::test]
    async fn test_extractor_success() {
        let db = Database::open_in_memory().expect("in memory db");
        let key = ApiKey {
            id: "key-1".to_string(),
            key: "sk-valid-key-999".to_string(),
            name: Some("Dev Key".to_string()),
            machine_id: None,
            is_active: true,
            created_at: Utc::now().to_rfc3339(),
            rate_limit_requests: 0,
            rate_limit_window_seconds: 60,
            token_limit_daily: 0,
            token_limit_monthly: 0,
            allowed_models: "".to_string(),
            blocked_models: "".to_string(),
            allowed_ips: "".to_string(),
            blocked_ips: "".to_string(),
            expires_at: "".to_string(),
            description: "".to_string(),
            owner_ref: None,
            max_concurrent_requests: 0,
            allowed_providers: "".to_string(),
            blocked_providers: "".to_string(),
            force_provider: "".to_string(),
            force_model: "".to_string(),
            allowed_endpoints: "".to_string(),
            blocked_endpoints: "".to_string(),
            ..Default::default()
        };
        db.save_api_key(&key).expect("save key");

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer sk-valid-key-999")
            .body(())
            .unwrap();

        let (mut parts, _) = request.into_parts();
        let extracted = AuthenticatedKey::from_request_parts(&mut parts, &db)
            .await
            .expect("extractor should succeed");

        assert_eq!(extracted.raw_key, "sk-valid-key-999");
        assert_eq!(extracted.key.id, "key-1");
    }

    #[tokio::test]
    async fn test_extractor_missing_key() {
        let db = Database::open_in_memory().expect("in memory db");
        let request = Request::builder().body(()).unwrap();
        let (mut parts, _) = request.into_parts();
        let err = AuthenticatedKey::from_request_parts(&mut parts, &db)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::MissingApiKey));
    }

    #[tokio::test]
    async fn test_extractor_invalid_key() {
        let db = Database::open_in_memory().expect("in memory db");
        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer sk-non-existent")
            .body(())
            .unwrap();
        let (mut parts, _) = request.into_parts();
        let err = AuthenticatedKey::from_request_parts(&mut parts, &db)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::InvalidApiKey));
    }

    #[test]
    fn test_verify_dashboard_jwt_valid() {
        use base64::Engine;
        let secret = "test-jwt-secret-12345";
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let payload = format!(
            r#"{{"authenticated":true,"exp":{}}}"#,
            chrono::Utc::now().timestamp() + 3600
        );

        let h_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header.as_bytes());
        let p_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        let signing_input = format!("{}.{}", h_b64, p_b64);

        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
        let sig = ring::hmac::sign(&key, signing_input.as_bytes());
        let s_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref());

        let token = format!("{}.{}.{}", h_b64, p_b64, s_b64);
        assert!(verify_dashboard_jwt(&token, secret));

        // Invalid secret
        assert!(!verify_dashboard_jwt(&token, "wrong-secret"));
    }

    #[test]
    fn test_verify_dashboard_jwt_expired() {
        use base64::Engine;
        let secret = "test-jwt-secret-12345";
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let payload = format!(
            r#"{{"authenticated":true,"exp":{}}}"#,
            chrono::Utc::now().timestamp() - 100
        );

        let h_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(header.as_bytes());
        let p_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        let signing_input = format!("{}.{}", h_b64, p_b64);

        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
        let sig = ring::hmac::sign(&key, signing_input.as_bytes());
        let s_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref());

        let token = format!("{}.{}.{}", h_b64, p_b64, s_b64);
        assert!(!verify_dashboard_jwt(&token, secret));
    }

    #[test]
    fn test_admin_api_token_header() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ADMIN_API_TOKEN", "super-secret-admin-key-999");
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-admin-key", "super-secret-admin-key-999".parse().unwrap());
        assert!(verify_admin_api_token(&headers));

        let mut bearer_headers = HeaderMap::new();
        bearer_headers.insert(header::AUTHORIZATION, "Bearer super-secret-admin-key-999".parse().unwrap());
        assert!(verify_admin_api_token(&bearer_headers));

        let mut wrong_headers = HeaderMap::new();
        wrong_headers.insert("x-admin-key", "wrong-key".parse().unwrap());
        assert!(!verify_admin_api_token(&wrong_headers));
        unsafe {
            std::env::remove_var("ADMIN_API_TOKEN");
        }
    }

    #[test]
    fn test_ninerouter_api_key_alias() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ADMIN_API_TOKEN");
            std::env::remove_var("ADMIN_KEY");
            std::env::remove_var("ROUTER_API_KEY");
            std::env::remove_var("NINE_ROUTER_API_KEY");
            std::env::set_var("NINEROUTER_API_KEY", "nr-alias-key-12345");
        }

        assert_eq!(resolve_configured_admin_token(), "nr-alias-key-12345");

        // x-admin-key
        let mut h1 = HeaderMap::new();
        h1.insert("x-admin-key", "nr-alias-key-12345".parse().unwrap());
        assert!(verify_admin_api_token(&h1));

        // x-router-key
        let mut h2 = HeaderMap::new();
        h2.insert("x-router-key", "nr-alias-key-12345".parse().unwrap());
        assert!(verify_admin_api_token(&h2));

        // x-api-key
        let mut h3 = HeaderMap::new();
        h3.insert("x-api-key", "nr-alias-key-12345".parse().unwrap());
        assert!(verify_admin_api_token(&h3));

        // Authorization: Bearer
        let mut h4 = HeaderMap::new();
        h4.insert(header::AUTHORIZATION, "Bearer nr-alias-key-12345".parse().unwrap());
        assert!(verify_admin_api_token(&h4));

        // Invalid key
        let mut h_err = HeaderMap::new();
        h_err.insert("x-admin-key", "invalid-key".parse().unwrap());
        assert!(!verify_admin_api_token(&h_err));

        unsafe {
            std::env::remove_var("NINEROUTER_API_KEY");
        }
    }

    #[test]
    fn test_nine_router_api_key_alias() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ADMIN_API_TOKEN");
            std::env::remove_var("ADMIN_KEY");
            std::env::remove_var("ROUTER_API_KEY");
            std::env::remove_var("NINEROUTER_API_KEY");
            std::env::set_var("NINE_ROUTER_API_KEY", "nine-router-alias-98765");
        }

        assert_eq!(resolve_configured_admin_token(), "nine-router-alias-98765");

        // x-admin-key
        let mut h1 = HeaderMap::new();
        h1.insert("x-admin-key", "nine-router-alias-98765".parse().unwrap());
        assert!(verify_admin_api_token(&h1));

        // x-router-key
        let mut h2 = HeaderMap::new();
        h2.insert("x-router-key", "nine-router-alias-98765".parse().unwrap());
        assert!(verify_admin_api_token(&h2));

        // Authorization: Bearer
        let mut h3 = HeaderMap::new();
        h3.insert(header::AUTHORIZATION, "Bearer nine-router-alias-98765".parse().unwrap());
        assert!(verify_admin_api_token(&h3));

        unsafe {
            std::env::remove_var("NINE_ROUTER_API_KEY");
        }
    }

    #[test]
    fn test_router_api_key_alias() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ADMIN_API_TOKEN");
            std::env::remove_var("ADMIN_KEY");
            std::env::remove_var("NINEROUTER_API_KEY");
            std::env::remove_var("NINE_ROUTER_API_KEY");
            std::env::set_var("ROUTER_API_KEY", "router-api-key-555");
        }

        assert_eq!(resolve_configured_admin_token(), "router-api-key-555");

        let mut h = HeaderMap::new();
        h.insert("x-router-key", "router-api-key-555".parse().unwrap());
        assert!(verify_admin_api_token(&h));

        unsafe {
            std::env::remove_var("ROUTER_API_KEY");
        }
    }

    // =========================================================================
    // Rate Limiting Unit Tests (Per-IP, Per-Model, Window Reset & Concurrency)
    // =========================================================================

    #[test]
    fn test_extract_client_ip_headers_priority_and_fallbacks() {
        use std::net::SocketAddr;

        // 1. cf-connecting-ip priority over everything
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "198.51.100.1".parse().unwrap());
        h.insert("x-forwarded-for", "203.0.113.195, 70.41.3.18".parse().unwrap());
        h.insert("x-real-ip", "192.0.2.1".parse().unwrap());
        let peer: SocketAddr = "10.0.0.1:12345".parse().unwrap();
        assert_eq!(extract_client_ip(&h, Some(peer)), "198.51.100.1");

        // 2. x-forwarded-for first IP priority over x-real-ip and peer_addr
        let mut h2 = HeaderMap::new();
        h2.insert("x-forwarded-for", "  203.0.113.195  , 70.41.3.18 ".parse().unwrap());
        h2.insert("x-real-ip", "192.0.2.1".parse().unwrap());
        assert_eq!(extract_client_ip(&h2, Some(peer)), "203.0.113.195");

        // 3. x-real-ip priority over peer_addr
        let mut h3 = HeaderMap::new();
        h3.insert("x-real-ip", "192.0.2.1:8080".parse().unwrap());
        assert_eq!(extract_client_ip(&h3, Some(peer)), "192.0.2.1");

        // 4. peer_addr when no headers present
        let h4 = HeaderMap::new();
        assert_eq!(extract_client_ip(&h4, Some(peer)), "10.0.0.1");

        // 5. Default fallback to 127.0.0.1 when nothing present
        assert_eq!(extract_client_ip(&h4, None), "127.0.0.1");

        // 6. Normalization with IPv6 addresses
        let mut h5 = HeaderMap::new();
        h5.insert("cf-connecting-ip", "2001:db8::1".parse().unwrap());
        assert_eq!(extract_client_ip(&h5, None), "2001:db8::1");
    }

    #[test]
    fn test_ip_rate_limiting_rpm_and_rps_throttling() {
        let mut cfg = RateLimitConfig::default();
        cfg.ip_rate_limit_enabled = true;
        cfg.ip_rpm = Some(3); // 3 requests per 60s
        cfg.ip_rps = Some(2); // 2 requests per 1s
        cfg.ip_window_secs = 60;

        let limiter = RateLimiter::new(cfg);
        let client_ip = "192.168.1.50";

        // First 2 requests within same second succeed
        assert!(limiter.check_ip(client_ip).is_ok());
        assert!(limiter.check_ip(client_ip).is_ok());

        // 3rd request in same second fails due to RPS limit (2 RPS)
        let rps_err = limiter.check_ip(client_ip);
        assert!(rps_err.is_err());
        if let Err(RateLimitError::IpRateLimited { ip, limit, quota_type, .. }) = rps_err {
            assert_eq!(ip, client_ip);
            assert_eq!(limit, 2);
            assert_eq!(quota_type, "RPS");
        } else {
            panic!("Expected RPS RateLimitError");
        }

        // Test RPM limit with RPS disabled
        let mut cfg2 = RateLimitConfig::default();
        cfg2.ip_rate_limit_enabled = true;
        cfg2.ip_rpm = Some(2);
        cfg2.ip_window_secs = 60;
        let limiter2 = RateLimiter::new(cfg2);

        assert!(limiter2.check_ip("10.1.1.1").is_ok());
        assert!(limiter2.check_ip("10.1.1.1").is_ok());

        let rpm_err = limiter2.check_ip("10.1.1.1");
        assert!(rpm_err.is_err());
        if let Err(RateLimitError::IpRateLimited { ip, limit, quota_type, retry_after, .. }) = rpm_err {
            assert_eq!(ip, "10.1.1.1");
            assert_eq!(limit, 2);
            assert_eq!(quota_type, "RPM");
            assert!(retry_after >= 1);
        } else {
            panic!("Expected RPM RateLimitError");
        }
    }

    #[test]
    fn test_ip_rate_limiting_allowlist_bypass() {
        let mut cfg = RateLimitConfig::default();
        cfg.ip_rate_limit_enabled = true;
        cfg.ip_rpm = Some(1);
        cfg.ip_allowlist = vec!["127.0.0.1".to_string(), "10.0.0.99".to_string()];

        let limiter = RateLimiter::new(cfg);

        // Allowlisted IP never gets throttled
        for _ in 0..10 {
            assert!(limiter.check_ip("127.0.0.1").is_ok());
            assert!(limiter.check_ip("10.0.0.99").is_ok());
        }

        // Non-allowlisted IP is throttled on request 2
        assert!(limiter.check_ip("192.168.1.10").is_ok());
        assert!(limiter.check_ip("192.168.1.10").is_err());
    }

    #[test]
    fn test_ip_rate_limiting_window_reset() {
        let mut cfg = RateLimitConfig::default();
        cfg.ip_rate_limit_enabled = true;
        cfg.ip_rpm = Some(1);
        let limiter = RateLimiter::new(cfg);

        assert!(limiter.check_ip("1.1.1.1").is_ok());
        assert!(limiter.check_ip("1.1.1.1").is_err());

        // Explicit reset enables immediate access
        limiter.reset_ip("1.1.1.1");
        assert!(limiter.check_ip("1.1.1.1").is_ok());
    }

    #[test]
    fn test_model_rate_limiting_rpm_and_tpm() {
        let mut cfg = RateLimitConfig::default();
        cfg.model_rate_limit_enabled = true;
        cfg.model_default_rpm = Some(2);
        cfg.model_default_tpm = Some(1000);

        let mut limits = std::collections::HashMap::new();
        limits.insert(
            "gpt-4o".to_string(),
            crate::config::ModelLimitConfig {
                rpm: Some(2),
                tpm: Some(500),
            },
        );
        cfg.model_limits = limits;

        let limiter = RateLimiter::new(cfg);

        // Check model RPM limit (gpt-4o limit: 2 RPM)
        assert!(limiter.check_model("gpt-4o", 100).is_ok());
        assert!(limiter.check_model("gpt-4o", 100).is_ok());

        let rpm_err = limiter.check_model("gpt-4o", 50);
        assert!(rpm_err.is_err());
        if let Err(RateLimitError::ModelRateLimited { model, limit, quota_type, retry_after, .. }) = rpm_err {
            assert_eq!(model, "gpt-4o");
            assert_eq!(limit, 2);
            assert_eq!(quota_type, "RPM");
            assert!(retry_after >= 1);
        } else {
            panic!("Expected Model RPM error");
        }

        // Check model TPM limit
        limiter.reset_model("gpt-4o");
        // Request 1: 300 tokens (allowed, total 300)
        assert!(limiter.check_model("gpt-4o", 300).is_ok());
        // Request 2: 300 tokens -> 300 + 300 = 600 > 500 limit (rejected)
        let tpm_err = limiter.check_model("gpt-4o", 300);
        assert!(tpm_err.is_err());
        if let Err(RateLimitError::ModelRateLimited { quota_type, limit, .. }) = tpm_err {
            assert_eq!(quota_type, "TPM");
            assert_eq!(limit, 500);
        } else {
            panic!("Expected Model TPM error");
        }
    }

    #[test]
    fn test_model_partition_isolation() {
        let mut cfg = RateLimitConfig::default();
        cfg.model_rate_limit_enabled = true;

        let mut limits = std::collections::HashMap::new();
        limits.insert(
            "gpt-4o".to_string(),
            crate::config::ModelLimitConfig {
                rpm: Some(1),
                tpm: Some(100000),
            },
        );
        limits.insert(
            "claude-3-7-sonnet".to_string(),
            crate::config::ModelLimitConfig {
                rpm: Some(5),
                tpm: Some(100000),
            },
        );
        limits.insert(
            "deepseek-r1".to_string(),
            crate::config::ModelLimitConfig {
                rpm: Some(5),
                tpm: Some(100000),
            },
        );
        cfg.model_limits = limits;

        let limiter = RateLimiter::new(cfg);

        // Exhaust gpt-4o (limit 1 RPM)
        assert!(limiter.check_model("gpt-4o", 100).is_ok());
        assert!(limiter.check_model("gpt-4o", 100).is_err());

        // Verify claude-3-7-sonnet and deepseek-r1 are completely unaffected!
        assert!(limiter.check_model("claude-3-7-sonnet", 100).is_ok());
        assert!(limiter.check_model("claude-3-7-sonnet", 100).is_ok());
        assert!(limiter.check_model("deepseek-r1", 100).is_ok());
        assert!(limiter.check_model("deepseek-r1", 100).is_ok());

        // Provider prefixes strip correctly ("openai/gpt-4o" matches "gpt-4o")
        assert!(limiter.check_model("openai/gpt-4o", 100).is_err());
    }

    #[test]
    fn test_rate_limiter_auto_cleanup() {
        let mut cfg = RateLimitConfig::default();
        cfg.ip_rate_limit_enabled = true;
        cfg.ip_rpm = Some(10);
        cfg.model_rate_limit_enabled = true;
        cfg.model_default_rpm = Some(10);

        let limiter = RateLimiter::new(cfg);

        // Track 2 IPs and 2 models
        assert!(limiter.check_ip("10.0.0.1").is_ok());
        assert!(limiter.check_ip("10.0.0.2").is_ok());
        assert!(limiter.check_model("gpt-4o", 10).is_ok());
        assert!(limiter.check_model("claude-3-7-sonnet", 10).is_ok());

        assert_eq!(limiter.tracked_ips_count(), 2);
        assert_eq!(limiter.tracked_models_count(), 2);

        // Calling cleanup with a long idle threshold (e.g., 100s) keeps active entries
        let (p_ip, p_mod) = limiter.cleanup(std::time::Duration::from_secs(100));
        assert_eq!(p_ip, 0);
        assert_eq!(p_mod, 0);
        assert_eq!(limiter.tracked_ips_count(), 2);
        assert_eq!(limiter.tracked_models_count(), 2);

        // Calling cleanup with zero duration prunes all idle entries
        let (p_ip2, p_mod2) = limiter.cleanup(std::time::Duration::from_nanos(0));
        assert_eq!(p_ip2, 2);
        assert_eq!(p_mod2, 2);
        assert_eq!(limiter.tracked_ips_count(), 0);
        assert_eq!(limiter.tracked_models_count(), 0);
    }

    #[test]
    fn test_rate_limiter_concurrent_safety() {
        use std::sync::Arc;
        let mut cfg = RateLimitConfig::default();
        cfg.ip_rate_limit_enabled = true;
        cfg.ip_rpm = Some(500);
        cfg.model_rate_limit_enabled = true;
        cfg.model_default_rpm = Some(500);

        let limiter = Arc::new(RateLimiter::new(cfg));
        let mut handles = Vec::new();

        // 16 concurrent threads each performing 50 operations
        for thread_idx in 0..16 {
            let lim = Arc::clone(&limiter);
            handles.push(std::thread::spawn(move || {
                let ip = format!("10.10.{}.1", thread_idx % 4);
                let model = if thread_idx % 2 == 0 { "gpt-4o" } else { "claude-3-7-sonnet" };
                for _ in 0..50 {
                    let _ = lim.check_ip(&ip);
                    let _ = lim.check_model(model, 10);
                }
            }));
        }

        for h in handles {
            h.join().expect("thread join succeeded");
        }

        assert!(limiter.tracked_ips_count() <= 4);
        assert!(limiter.tracked_models_count() <= 2);
    }

    #[test]
    fn test_auth_error_rate_limiting_response_format() {
        use axum::response::IntoResponse;

        let err = AuthError::IpRateLimitExceeded {
            ip: "203.0.113.5".to_string(),
            limit: 60,
            window_seconds: 60,
            retry_after: 12,
            quota_type: "RPM",
        };

        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "12"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap(),
            "application/json"
        );
    }
}
