use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use rust_embed::RustEmbed;
use serde_json::json;

#[derive(RustEmbed)]
#[folder = "ui/"]
pub struct Assets;

/// Serves single-binary embedded assets from `rust/app/ui/` (authentic Next.js Dashboard).
/// Supports direct asset lookups, HTML page mappings, and SPA client-side fallback.
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

    // 1. Direct asset match (e.g., _next/static/..., assets/..., fonts, images, css)
    if let Some(file) = Assets::get(path) {
        let content_type = if path.ends_with(".html") {
            HeaderValue::from_static("text/html; charset=utf-8")
        } else {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            HeaderValue::from_str(mime.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
        };

        let cache_header = if path.starts_with("_next/static/") || path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };

        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, HeaderValue::from_static(cache_header))
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // 2. Direct page HTML match (e.g. /dashboard -> dashboard.html, /dashboard/providers -> dashboard/providers.html)
    let clean_path = path.trim_end_matches('/');
    let html_path = format!("{}.html", clean_path);
    if let Some(file) = Assets::get(&html_path) {
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // 3. Nested index.html match (e.g. /dashboard/ -> dashboard/index.html)
    let index_subpath = format!("{}/index.html", clean_path);
    if let Some(file) = Assets::get(&index_subpath) {
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from(file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // 4. If path has a file extension and was not found, return 404 (don't serve HTML for missing files)
    if path.contains('.') && !path.ends_with(".html") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "Asset not found",
        )
            .into_response();
    }

    // 5. SPA client-side routing fallback: try dashboard.html first, then index.html
    if let Some(dashboard_file) = Assets::get("dashboard.html") {
        Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from(dashboard_file.data))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else if let Some(index_file) = Assets::get("index.html") {
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
            "Embedded UI not found",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_authentic_dashboard_routes() {
        // 1. Root / should return HTML
        let resp = static_handler("/".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");

        // 2. /dashboard should map to dashboard.html
        let resp = static_handler("/dashboard".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("9Router") || body_str.contains("dashboard"));

        // 3. /dashboard/providers should map to dashboard/providers.html
        let resp = static_handler("/dashboard/providers".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");

        // 4. /dashboard/combos should map to dashboard/combos.html
        let resp = static_handler("/dashboard/combos".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");

        // 5. /login should map to login.html
        let resp = static_handler("/login".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");

        // 6. Unknown API route should 404 with JSON
        let resp = static_handler("/api/unknown".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(resp.headers().get("content-type").unwrap(), "application/json");
    }
}
