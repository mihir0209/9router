use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::db::{ApiKey, Database, KeyValidationError};

/// Standard auth error representation aligning with OpenAI error responses.
#[derive(Debug, Clone)]
pub enum AuthError {
    MissingApiKey,
    InvalidApiKey,
    DisabledApiKey,
    ExpiredApiKey,
    RateLimitExceeded { limit: i64, window_seconds: i64 },
    ModelForbidden(String),
    Internal(String),
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message, error_type): (StatusCode, String, &'static str) = match self {
            Self::MissingApiKey => (
                StatusCode::UNAUTHORIZED,
                "Missing API key. Provide via 'Authorization: Bearer <key>' or 'x-api-key' header.".to_string(),
                "authentication_error",
            ),
            Self::InvalidApiKey => (
                StatusCode::UNAUTHORIZED,
                "Invalid API key provided.".to_string(),
                "authentication_error",
            ),
            Self::DisabledApiKey => (
                StatusCode::UNAUTHORIZED,
                "The provided API key is disabled.".to_string(),
                "authentication_error",
            ),
            Self::ExpiredApiKey => (
                StatusCode::UNAUTHORIZED,
                "The provided API key has expired.".to_string(),
                "authentication_error",
            ),
            Self::RateLimitExceeded {
                limit,
                window_seconds,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Rate limit exceeded: max {} requests per {}s.",
                    limit, window_seconds
                ),
                "rate_limit_error",
            ),
            Self::ModelForbidden(reason) => {
                (StatusCode::FORBIDDEN, reason, "permission_denied")
            }
            Self::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                msg,
                "internal_server_error",
            ),
        };

        let body = Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": null,
                "code": status.as_u16(),
            }
        }));

        let mut res = (status, body).into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            res.headers_mut()
                .insert(header::RETRY_AFTER, "1".parse().unwrap());
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
pub fn resolve_configured_admin_token() -> String {
    if let Ok(tok) = std::env::var("ADMIN_API_TOKEN").or_else(|_| std::env::var("ADMIN_KEY")) {
        let trimmed = tok.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
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
                    if k.trim() == "ADMIN_API_TOKEN" || k.trim() == "ADMIN_KEY" {
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
        if !trimmed.is_empty() {
            configured_tokens.push(trimmed);
        }
    }
    if let Ok(router_key) = std::env::var("ROUTER_API_KEY") {
        let trimmed = router_key.trim().to_string();
        if !trimmed.is_empty() {
            configured_tokens.push(trimmed);
        }
    }

    if configured_tokens.is_empty() {
        return false;
    }

    // 1. Check x-admin-key, x-internal-token, x-router-key headers
    for header_name in &["x-admin-key", "x-internal-token", "x-router-key", "x-9r-cli-token"] {
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
    // Check if requireLogin is explicitly disabled
    if let Ok(settings_map) = db.load_settings() {
        if settings_map.get("requireLogin").and_then(|v| v.as_bool()) == Some(false) {
            return Ok(());
        }
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
    use axum::http::Request;
    use chrono::Utc;

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
    }
}
