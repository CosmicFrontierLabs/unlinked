//! Trunk intentionally keeps worker filenames stable, including their JS/WASM
//! dependencies. Revalidate all three across deployments; hashed assets retain
//! memory-serve's long cache policy and ETags remain usable for HTTP 304.
use axum::{
    extract::Request,
    http::{header, HeaderValue},
    middleware::Next,
    response::Response,
};
pub async fn revalidate_worker(request: Request, next: Next) -> Response {
    let worker = matches!(
        request.uri().path(),
        "/unlinked-diagnostics-worker_loader.js"
            | "/unlinked-diagnostics-worker.js"
            | "/unlinked-diagnostics-worker_bg.wasm"
    );
    let mut response = next.run(request).await;
    if worker {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::StatusCode, middleware, routing::get, Router};
    use tower::ServiceExt;
    async fn asset(request: Request) -> Response {
        let mut response = Response::new(Body::empty());
        if request.headers().contains_key(header::IF_NONE_MATCH) {
            *response.status_mut() = StatusCode::NOT_MODIFIED;
        }
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000"),
        );
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_static("\"content-version\""),
        );
        response
    }
    #[tokio::test]
    async fn stable_worker_assets_revalidate_but_other_assets_keep_cache_policy() {
        let app = Router::new()
            .route("/*asset", get(asset))
            .layer(middleware::from_fn(revalidate_worker));
        for name in [
            "unlinked-diagnostics-worker_loader.js",
            "unlinked-diagnostics-worker.js",
            "unlinked-diagnostics-worker_bg.wasm",
            "frontend-abcd.js",
            "frontend-abcd_bg.wasm",
            "other/unlinked-diagnostics-worker.js",
        ] {
            for conditional in [false, true] {
                let mut request = Request::builder().uri(format!("/{name}?ignored=1"));
                if conditional {
                    request = request.header(header::IF_NONE_MATCH, "\"content-version\"");
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                let expected = if name.starts_with("unlinked-diagnostics-worker") {
                    "no-cache"
                } else {
                    "public, max-age=31536000"
                };
                assert_eq!(response.headers()[header::CACHE_CONTROL], expected);
                assert_eq!(response.headers()[header::ETAG], "\"content-version\"");
                assert_eq!(
                    response.status(),
                    if conditional {
                        StatusCode::NOT_MODIFIED
                    } else {
                        StatusCode::OK
                    }
                );
            }
        }
    }
}
