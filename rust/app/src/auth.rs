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
}
