//! Server-side sessions and the [`CurrentUser`] extractor.
//!
//! The session cookie holds a random 256-bit token, encrypted and
//! authenticated with the server's cookie key. The `sessions` table stores only
//! the token's SHA-256 alongside an expiry, so sessions can be revoked
//! server-side (logout) and a database leak does not yield usable cookies.

use std::sync::Arc;

use axum::async_trait;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use diesel::prelude::*;
use diesel::PgConnection;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tower_cookies::cookie::{time, SameSite};
use tower_cookies::{Cookie, Cookies};
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::{NewSession, User};
use crate::origin::origin_allowed;
use crate::schema::{sessions, users};
use crate::AppState;

pub const SESSION_COOKIE: &str = "unlinked_session";

/// Generate a fresh session token (hex of 32 random bytes).
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Insert a session row for `user_id` and return the raw token for the cookie.
pub fn create_session(
    conn: &mut PgConnection,
    user_id: Uuid,
    ttl: chrono::Duration,
) -> QueryResult<String> {
    let token = new_token();
    diesel::insert_into(sessions::table)
        .values(NewSession {
            user_id,
            token_hash: hash_token(&token),
            expires_at: Utc::now() + ttl,
        })
        .execute(conn)?;
    diesel::delete(sessions::table.filter(sessions::expires_at.lt(Utc::now()))).execute(conn)?;
    Ok(token)
}

/// The user owning a live session with this token, if any.
pub fn session_user(conn: &mut PgConnection, token: &str) -> QueryResult<Option<User>> {
    sessions::table
        .inner_join(users::table)
        .filter(sessions::token_hash.eq(hash_token(token)))
        .filter(sessions::expires_at.gt(Utc::now()))
        .select(User::as_select())
        .first(conn)
        .optional()
}

pub fn delete_session(conn: &mut PgConnection, token: &str) -> QueryResult<usize> {
    diesel::delete(sessions::table.filter(sessions::token_hash.eq(hash_token(token)))).execute(conn)
}

/// Set the session cookie: HttpOnly, SameSite=Lax, Secure outside dev mode,
/// lifetime matching the server-side expiry.
pub fn set_session_cookie(cookies: &Cookies, state: &AppState, token: String) {
    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::seconds(
        state.config.session_ttl.num_seconds(),
    ));
    cookies.private(&state.config.cookie_key).add(cookie);
}

/// The raw session token from the request's cookie, if present and authentic.
pub fn session_token(cookies: &Cookies, state: &AppState) -> Option<String> {
    cookies
        .private(&state.config.cookie_key)
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_string())
}

pub fn clear_session_cookie(cookies: &Cookies, state: &AppState) {
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookies.private(&state.config.cookie_key).remove(cookie);
}

/// The signed-in user. Rejects with 401 when there is no valid session.
pub struct CurrentUser(pub User);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|(_, msg)| ApiError::Internal(msg.to_string()))?;
        let token = session_token(&cookies, state).ok_or(ApiError::Unauthorized)?;
        let user = state
            .db(move |conn| Ok(session_user(conn, &token)?))
            .await?
            .ok_or(ApiError::Unauthorized)?;
        Ok(CurrentUser(user))
    }
}

/// The signed-in user for a WebSocket upgrade.
///
/// Browsers attach cookies to cross-site WebSocket handshakes and the
/// handshake is not subject to CORS, so a session cookie alone would let any
/// site open a socket as the user (cross-site WebSocket hijacking). This
/// extractor therefore also requires an `Origin` header matching
/// [`origin_allowed`]: 403 when it is missing or foreign, then 401 without a
/// valid session. Use it (directly, or via [`require_ws_user`]) on every
/// WebSocket route that acts as a user; for project-scoped sockets, follow it
/// with [`crate::access::resolve_project_access`].
pub struct WsUser(pub User);

#[async_trait]
impl FromRequestParts<Arc<AppState>> for WsUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let origin = parts
            .headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        if !origin.is_some_and(|o| origin_allowed(state, o)) {
            tracing::warn!(?origin, "WebSocket upgrade from disallowed origin");
            return Err(ApiError::CrossOrigin);
        }
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        Ok(WsUser(user))
    }
}

/// Route middleware applying [`WsUser`] to handlers that cannot take
/// extractors (such as `ws_bridge` endpoints). On success the [`User`] is
/// available to the handler as `Extension<User>`.
pub async fn require_ws_user(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    match WsUser::from_request_parts(&mut parts, &state).await {
        Ok(WsUser(user)) => {
            parts.extensions.insert(user);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hash_stably() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(hash_token(&a), hash_token(&a));
        assert_ne!(hash_token(&a), a);
    }
}
