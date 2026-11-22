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

fn url_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

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

    let decoded_path = url_decode(raw_path);
    let path = decoded_path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    // 1. Direct asset match (e.g., _next/static/..., assets/..., fonts, images, css)
    // Check both decoded path and raw path
    let file_opt = Assets::get(path).or_else(|| Assets::get(raw_path.trim_start_matches('/')));
    if let Some(file) = file_opt {
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

    // 2. React Server Component (RSC) payload request (Next.js client-side navigation)
    let is_rsc_request = uri.query().map(|q| q.contains("_rsc=")).unwrap_or(false)
        || path.ends_with(".rsc");
    let clean_path = path.trim_end_matches('/').trim_end_matches(".rsc");

    if is_rsc_request {
        let rsc_path = format!("{}.rsc", clean_path);
        let dashboard_rsc_path = format!("dashboard/{}.rsc", clean_path);
        if let Some(file) = Assets::get(&rsc_path).or_else(|| Assets::get(&dashboard_rsc_path)) {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, HeaderValue::from_static("text/x-component"))
                .header(
                    header::VARY,
                    HeaderValue::from_static("RSC, Next-Router-State-Tree, Next-Router-Prefetch, Next-Url"),
                )
                .header(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=3600, stale-while-revalidate=86400"),
                )
                .body(Body::from(file.data))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        // Dynamic provider detail RSC fallback
        if clean_path.starts_with("dashboard/providers/") {
            if let Some(file) = Assets::get("dashboard/providers/openai.rsc") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, HeaderValue::from_static("text/x-component"))
                    .header(
                        header::VARY,
                        HeaderValue::from_static("RSC, Next-Router-State-Tree, Next-Router-Prefetch, Next-Url"),
                    )
                    .header(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=3600, stale-while-revalidate=86400"),
                    )
                    .body(Body::from(file.data))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }

        // Prevent RSC requests from falling through to HTML pages
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, HeaderValue::from_static("text/x-component"))
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response());
    }

    // 3. Direct page HTML match (e.g. /dashboard -> dashboard.html, /dashboard/providers -> dashboard/providers.html)
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

    // 4. Dynamic route template fallbacks
    // Providers: if /dashboard/providers/:id is not an exact pre-rendered file, serve the provider detail template
    if clean_path.starts_with("dashboard/providers/") {
        if let Some(file) = Assets::get("dashboard/providers/openai.html") {
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
    }

    // CLI Tools: if /dashboard/cli-tools/:toolId is not an exact pre-rendered file, serve cli-tools template
    if clean_path.starts_with("dashboard/cli-tools/") {
        if let Some(file) = Assets::get("dashboard/cli-tools/claude.html") {
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
    }

    // 5. Nested index.html match (e.g. /dashboard/ -> dashboard/index.html)
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

    // 6. If path has a file extension and was not found, return 404 (don't serve HTML for missing files)
    if path.contains('.') && !path.ends_with(".html") && !path.ends_with(".rsc") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "Asset not found",
        )
            .into_response();
    }

    // 7. SPA client-side routing fallback: try dashboard.html first, then index.html
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

        // 7. Dynamic provider detail HTML
        let resp = static_handler("/dashboard/providers/openai".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");

        // 8. Dynamic provider detail RSC payload
        let resp = static_handler("/dashboard/providers/openai?_rsc=1".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/x-component");

        // 9. Percent-encoded chunk lookup for bracketed routes
        let resp = static_handler(
            "/_next/static/chunks/app/(dashboard)/dashboard/providers/%5Bid%5D/page-dbb8cea5c3bf1206.js"
                .parse()
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
