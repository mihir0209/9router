use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use rust_embed::RustEmbed;
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
pub struct Assets;

/// Serves single-binary embedded assets from `web/dist/`.
/// Fallback for client-side routing serves `index.html`.
pub async fn static_handler(uri: Uri) -> Response {
    let raw_path = uri.path();

    // API 404s must return standard JSON error instead of SPA HTML fallback
    if raw_path.starts_with("/api/") || raw_path.starts_with("/v1/") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": "Not Found"})),
        )
            .into_response();
    }

    let path = raw_path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // 1. Direct asset match (e.g., assets/index-*.js, fonts, images)
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache_header = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };

        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static(cache_header))
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // 2. If path has a file extension and was not found, return 404 (don't serve HTML)
    if path.contains('.') && !path.ends_with(".html") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "Asset not found",
        )
            .into_response();
    }

    // 3. SPA client-side routing fallback: index.html
    if let Some(index_file) = Assets::get("index.html") {
        Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from(index_file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "Embedded UI index.html not found",
        )
            .into_response()
    }
}
