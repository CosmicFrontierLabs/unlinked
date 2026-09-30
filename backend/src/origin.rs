//! Same-origin enforcement (CSRF defence).
//!
//! The frontend is served by this binary, so legitimate browser requests are
//! always same-origin and no CORS is configured. SameSite=Lax cookies alone
//! are not enough: they still flow on requests from hostile *same-site*
//! origins (sibling subdomains), and CORS never prevents a request's side
//! effects, only reading its response. So every state-changing request is
//! checked here against `PUBLIC_URL`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use url::Url;

use crate::error::ApiError;
use crate::session::SESSION_COOKIE;
use crate::AppState;

/// Whether `origin` is this deployment's own origin (`PUBLIC_URL`), or, in
/// dev mode only, any localhost origin.
pub fn origin_allowed(state: &AppState, origin: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    if url.origin() == state.config.public_url.origin() {
        return true;
    }
    state.dev_mode
        && matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// Where a request claims to come from: `Origin`, else the origin of
/// `Referer`. `None` when neither header is present.
fn request_origin(headers: &HeaderMap) -> Option<String> {
    if let Some(origin) = headers.get(header::ORIGIN) {
        return Some(origin.to_str().unwrap_or_default().to_string());
    }
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    Some(
        Url::parse(referer)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_default(),
    )
}

fn has_session_cookie(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|c| {
            c.trim()
                .split_once('=')
                .is_some_and(|(name, _)| name == SESSION_COOKIE)
        })
}

/// Router middleware rejecting cross-origin state-changing requests.
///
/// Safe methods (GET, HEAD, OPTIONS, TRACE) pass. Other methods must carry an
/// `Origin` (or, failing that, `Referer`) allowed by [`origin_allowed`]. A
/// request with neither header is rejected when it carries a session cookie,
/// since it could then act as a user; without one it cannot, and passes on to
/// authentication.
pub async fn require_same_origin(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if request.method().is_safe() {
        return next.run(request).await;
    }
    let allowed = match request_origin(request.headers()) {
        Some(origin) => origin_allowed(&state, &origin),
        None => !has_session_cookie(request.headers()),
    };
    if allowed {
        next.run(request).await
    } else {
        tracing::warn!(
            method = %request.method(),
            uri = %request.uri(),
            origin = ?request.headers().get(header::ORIGIN),
            referer = ?request.headers().get(header::REFERER),
            "rejected cross-origin request"
        );
        ApiError::CrossOrigin.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{offline_state, test_config, TEST_PUBLIC_URL};
    use axum::http::HeaderValue;

    #[test]
    fn origin_must_match_public_url() {
        let state = offline_state(test_config());
        assert!(origin_allowed(&state, TEST_PUBLIC_URL));
        assert!(origin_allowed(&state, "https://unlinked.example.com:443"));
        for bad in [
            "http://unlinked.example.com",
            "https://unlinked.example.com:8443",
            "https://evil.example.com",
            "https://sub.unlinked.example.com",
            "https://unlinked.example.com.evil.org",
            "http://localhost:3000",
            "null",
            "",
        ] {
            assert!(!origin_allowed(&state, bad), "allowed {bad:?}");
        }
    }

    #[test]
    fn dev_mode_also_allows_localhost() {
        let mut dev = Arc::try_unwrap(offline_state(test_config())).ok().unwrap();
        dev.dev_mode = true;
        assert!(origin_allowed(&dev, "http://localhost:8080"));
        assert!(origin_allowed(&dev, "http://127.0.0.1:3000"));
        assert!(origin_allowed(&dev, "http://[::1]:3000"));
        assert!(!origin_allowed(&dev, "http://localhost.evil.org"));
        assert!(!origin_allowed(&dev, "file://localhost/x"));
    }

    #[test]
    fn request_origin_prefers_origin_then_referer() {
        let mut headers = HeaderMap::new();
        assert_eq!(request_origin(&headers), None);
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://unlinked.example.com/p/123?x=1"),
        );
        assert_eq!(
            request_origin(&headers).as_deref(),
            Some("https://unlinked.example.com")
        );
        headers.insert(header::ORIGIN, HeaderValue::from_static("https://evil.com"));
        assert_eq!(
            request_origin(&headers).as_deref(),
            Some("https://evil.com")
        );
    }

    #[test]
    fn detects_session_cookie_by_name() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
        assert!(!has_session_cookie(&headers));
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("x=1; unlinked_session=abc"),
        );
        assert!(has_session_cookie(&headers));
    }
}
