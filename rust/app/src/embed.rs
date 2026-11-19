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

pub async fn static_handler(uri: Uri) -> Response {
    let raw_path = uri.path();

    // API 404s should return JSON rather than SPA fallback
    if raw_path.starts_with("/api/") || raw_path.starts_with("/v1/") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({"error": "Not Found"})),
        )
            .into_response();
    }

    let path = raw_path.trim_start_matches('/');

    // Empty path -> index.html
    let path = if path.is_empty() { "index.html" } else { path };

    // Try finding exact file in embedded assets
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            )
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // If file has a file extension and wasn't found, return 404
    if path.contains('.') && !path.ends_with(".html") {
        return (StatusCode::NOT_FOUND, "Asset not found").into_response();
    }

    // SPA client-side routing fallback: index.html
    if let Some(index_file) = Assets::get("index.html") {
        Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )
            .body(Body::from(index_file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        (StatusCode::NOT_FOUND, "index.html not found in assets").into_response()
    }
}
