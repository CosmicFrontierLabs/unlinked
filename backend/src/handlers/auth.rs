//! Login, logout and `/api/me`.
//!
//! OAuth uses the authorization-code flow with PKCE. `login` stores a random
//! `state` and the PKCE verifier in a short-lived encrypted HttpOnly cookie
//! scoped to the callback path; `callback` rejects the request unless the
//! returned `state` matches that cookie, then exchanges the code (with the
//! verifier), requires a provider-verified email, applies
//! `ALLOWED_EMAIL_DOMAINS`, and starts a server-side session.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use diesel::prelude::*;
use diesel::PgConnection;
use oauth2::TokenResponse;
use oauth2::{AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, Scope};
use serde::{Deserialize, Serialize};
use shared::{AuthProvidersResponse, UserInfo};
use tower_cookies::cookie::{time, SameSite};
use tower_cookies::{Cookie, Cookies};

use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::models::{NewUser, NewUserIdentity, User};
use crate::oauth::{self, Provider};
use crate::schema::{user_identities, users};
use crate::session::{self, CurrentUser};
use crate::AppState;

pub const OAUTH_FLOW_COOKIE: &str = "unlinked_oauth";
/// The flow cookie is only ever sent to the callback endpoints.
pub const OAUTH_CALLBACK_PATH: &str = "/api/auth/callback";
const OAUTH_FLOW_TTL_MINUTES: i64 = 10;

const DEV_USER_EMAIL: &str = "dev@localhost";
const DEV_PROVIDER: &str = "dev";

/// Where a browser lands after signing in.
const AFTER_LOGIN: &str = "/";
/// Login page; receives `?error=<code>` when a login is refused.
const LOGIN_PAGE: &str = "/login";

/// Contents of the OAuth flow cookie.
#[derive(Debug, Serialize, Deserialize)]
pub struct OAuthFlow {
    pub provider: String,
    pub state: String,
    pub pkce_verifier: String,
}

pub async fn providers(State(state): State<Arc<AppState>>) -> Json<AuthProvidersResponse> {
    Json(AuthProvidersResponse {
        providers: state
            .config
            .oauth
            .enabled()
            .into_iter()
            .map(|p| p.key().to_string())
            .collect(),
        dev_mode: state.dev_mode,
    })
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    cookies: Cookies,
) -> ApiResult<Response> {
    let provider = Provider::from_key(&provider).ok_or(ApiError::NotFound)?;
    let Some(client) = state.config.oauth.client(provider) else {
        if state.dev_mode {
            return Ok(Redirect::temporary("/api/auth/dev-login").into_response());
        }
        return Err(ApiError::NotFound);
    };

    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf) = provider
        .scopes()
        .iter()
        .fold(client.authorize_url(CsrfToken::new_random), |req, s| {
            req.add_scope(Scope::new(s.to_string()))
        })
        .set_pkce_challenge(challenge)
        .url();

    let flow = OAuthFlow {
        provider: provider.key().to_string(),
        state: csrf.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
    };
    let value = serde_json::to_string(&flow)
        .map_err(|e| ApiError::Internal(format!("encode oauth flow: {e}")))?;
    let mut cookie = Cookie::new(OAUTH_FLOW_COOKIE, value);
    cookie.set_path(OAUTH_CALLBACK_PATH);
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::minutes(OAUTH_FLOW_TTL_MINUTES));
    cookies.private(&state.config.cookie_key).add(cookie);

    Ok(Redirect::temporary(url.as_str()).into_response())
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Read and clear the flow cookie. It is single-use either way.
fn take_flow(cookies: &Cookies, state: &AppState) -> Option<OAuthFlow> {
    let jar = cookies.private(&state.config.cookie_key);
    let flow = jar
        .get(OAUTH_FLOW_COOKIE)
        .and_then(|c| serde_json::from_str(c.value()).ok());
    let mut removal = Cookie::new(OAUTH_FLOW_COOKIE, "");
    removal.set_path(OAUTH_CALLBACK_PATH);
    jar.remove(removal);
    flow
}

pub async fn callback(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
    cookies: Cookies,
) -> ApiResult<Response> {
    let provider = Provider::from_key(&provider).ok_or(ApiError::NotFound)?;
    let client = state
        .config
        .oauth
        .client(provider)
        .ok_or(ApiError::NotFound)?;

    let flow = take_flow(&cookies, &state).ok_or_else(|| {
        tracing::warn!(
            provider = provider.key(),
            "OAuth callback without flow cookie"
        );
        ApiError::Forbidden
    })?;
    if flow.provider != provider.key() || query.state.as_deref() != Some(flow.state.as_str()) {
        tracing::warn!(provider = provider.key(), "OAuth callback state mismatch");
        return Err(ApiError::Forbidden);
    }
    if let Some(error) = query.error {
        return Err(ApiError::BadRequest(format!(
            "login not completed: {error}"
        )));
    }
    let code = query
        .code
        .ok_or_else(|| ApiError::BadRequest("missing authorization code".to_string()))?;

    let http = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ApiError::Internal(format!("build OAuth HTTP client: {e}")))?;
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
        .request_async(&http)
        .await
        .map_err(|e| ApiError::Internal(format!("OAuth code exchange failed: {e}")))?;

    let identity = oauth::fetch_identity(provider, token.access_token().secret()).await?;
    let Some(email) = identity.email.clone() else {
        tracing::warn!(
            provider = provider.key(),
            "login refused: no verified email"
        );
        return Ok(login_error("email_not_verified"));
    };
    if !state.config.email_allowed(&email) {
        tracing::warn!(provider = provider.key(), %email, "login refused: domain not allowed");
        return Ok(login_error("domain_not_allowed"));
    }

    let ttl = state.config.session_ttl;
    let token = state
        .db(move |conn| {
            sign_in(
                conn,
                (identity.provider.key(), &identity.subject),
                &email,
                identity.name.as_deref(),
                identity.avatar_url.as_deref(),
                ttl,
            )
        })
        .await?;
    session::set_session_cookie(&cookies, &state, token);
    Ok(Redirect::to(AFTER_LOGIN).into_response())
}

fn login_error(code: &str) -> Response {
    Redirect::to(&format!("{LOGIN_PAGE}?error={code}")).into_response()
}

/// Sign in the local dev user. Only routed in `--dev-mode`.
pub async fn dev_login(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
) -> ApiResult<Response> {
    if !state.dev_mode {
        return Err(ApiError::NotFound);
    }
    let ttl = state.config.session_ttl;
    let token = state
        .db(move |conn| {
            sign_in(
                conn,
                (DEV_PROVIDER, "dev"),
                DEV_USER_EMAIL,
                Some("Dev User"),
                None,
                ttl,
            )
        })
        .await?;
    session::set_session_cookie(&cookies, &state, token);
    Ok(Redirect::to(AFTER_LOGIN).into_response())
}

/// Resolve the user behind a login, audit it, and open a session. Returns the
/// raw session token for the cookie.
fn sign_in(
    conn: &mut PgConnection,
    (provider, subject): (&str, &str),
    email: &str,
    name: Option<&str>,
    avatar_url: Option<&str>,
    ttl: chrono::Duration,
) -> ApiResult<String> {
    let user = resolve_user(conn, (provider, subject), email, name, avatar_url)?;
    audit::record(
        conn,
        user.id,
        None,
        None,
        "auth.login",
        serde_json::json!({ "provider": provider }),
    )?;
    Ok(session::create_session(conn, user.id, ttl)?)
}

/// Find or create the user behind a login.
///
/// A known `(provider, subject)` maps straight to its user. Otherwise the
/// identity is linked to the user with the same (provider-verified) email,
/// or a new user is created. Profile fields are refreshed on every login.
pub fn resolve_user(
    conn: &mut PgConnection,
    (provider, subject): (&str, &str),
    email: &str,
    name: Option<&str>,
    avatar_url: Option<&str>,
) -> QueryResult<User> {
    let email = email.to_lowercase();
    conn.transaction(|conn| {
        let existing = user_identities::table
            .inner_join(users::table)
            .filter(user_identities::provider.eq(provider))
            .filter(user_identities::subject.eq(subject))
            .select(User::as_select())
            .first(conn)
            .optional()?;
        let user = match existing {
            Some(user) => user,
            None => {
                let user = match users::table
                    .filter(users::email.eq(&email))
                    .select(User::as_select())
                    .first(conn)
                    .optional()?
                {
                    Some(user) => user,
                    None => diesel::insert_into(users::table)
                        .values(NewUser {
                            email: &email,
                            name,
                            avatar_url,
                        })
                        .returning(User::as_returning())
                        .get_result(conn)?,
                };
                diesel::insert_into(user_identities::table)
                    .values(NewUserIdentity {
                        user_id: user.id,
                        provider,
                        subject,
                    })
                    .execute(conn)?;
                user
            }
        };
        diesel::update(users::table.find(user.id))
            .set((
                users::name.eq(name.or(user.name.as_deref())),
                users::avatar_url.eq(avatar_url.or(user.avatar_url.as_deref())),
                users::last_login_at.eq(chrono::Utc::now()),
            ))
            .returning(User::as_returning())
            .get_result(conn)
    })
}

pub async fn logout(State(state): State<Arc<AppState>>, cookies: Cookies) -> ApiResult<StatusCode> {
    if let Some(token) = session::session_token(&cookies, &state) {
        state
            .db(move |conn| Ok(session::delete_session(conn, &token)?))
            .await?;
    }
    session::clear_session_cookie(&cookies, &state);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(CurrentUser(user): CurrentUser) -> Json<UserInfo> {
    Json(user.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_app;
    use crate::config::oauth_client;
    use crate::test_support::{
        offline_state, private_cookie_header, send, test_config, TEST_PUBLIC_URL,
    };
    use axum::body::Body;
    use axum::http::{header, Method};
    use tower_cookies::Key;

    fn oauth_state() -> Arc<AppState> {
        let mut config = test_config();
        config.oauth.google = Some(
            oauth_client(
                Provider::Google,
                "client-id".to_string(),
                "client-secret".to_string(),
                "http://localhost:3000/api/auth/callback/google".to_string(),
            )
            .unwrap(),
        );
        offline_state(config)
    }

    fn flow_cookie(state: &AppState, provider: &str, csrf: &str) -> String {
        let flow = OAuthFlow {
            provider: provider.to_string(),
            state: csrf.to_string(),
            pkce_verifier: "verifier".to_string(),
        };
        private_cookie_header(
            &state.config.cookie_key,
            OAUTH_FLOW_COOKIE,
            &serde_json::to_string(&flow).unwrap(),
        )
    }

    async fn get(
        app: &axum::Router,
        uri: &str,
        cookie: Option<&str>,
    ) -> crate::test_support::TestResponse {
        let headers: Vec<_> = cookie
            .map(|c| (header::COOKIE, c.to_string()))
            .into_iter()
            .collect();
        send(app, Method::GET, uri, &headers, Body::empty()).await
    }

    #[tokio::test]
    async fn protected_routes_require_a_session() {
        let app = build_app(offline_state(test_config()));
        let forged = private_cookie_header(&Key::generate(), crate::session::SESSION_COOKIE, "x");
        let project = uuid::Uuid::new_v4();
        let routes = [
            (Method::GET, "/api/me".to_string()),
            (Method::GET, "/api/orgs".to_string()),
            (Method::POST, "/api/orgs".to_string()),
            (Method::GET, "/api/projects".to_string()),
            (Method::GET, format!("/api/projects/{project}")),
            (Method::GET, format!("/api/projects/{project}/files")),
            (
                Method::POST,
                format!("/api/projects/{project}/files?path=a.slx"),
            ),
        ];
        for (method, uri) in routes {
            for cookie in [
                None,
                Some("unlinked_session=plaintext"),
                Some(forged.as_str()),
            ] {
                let mut headers = vec![(header::ORIGIN, TEST_PUBLIC_URL.to_string())];
                if let Some(cookie) = cookie {
                    headers.push((header::COOKIE, cookie.to_string()));
                }
                let resp = send(&app, method.clone(), &uri, &headers, Body::empty()).await;
                assert_eq!(
                    resp.status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} {cookie:?}"
                );
                let err: shared::ErrorResponse = resp.json();
                assert_eq!(err.error, "authentication required");
            }
        }
    }

    #[tokio::test]
    async fn providers_lists_configured_clients() {
        let app = build_app(oauth_state());
        let resp = get(&app, "/api/auth/providers", None).await;
        assert_eq!(resp.status, StatusCode::OK);
        let body: AuthProvidersResponse = resp.json();
        assert_eq!(body.providers, vec!["google".to_string()]);
        assert!(!body.dev_mode);
    }

    #[tokio::test]
    async fn login_redirects_with_state_pkce_and_flow_cookie() {
        let app = build_app(oauth_state());
        let resp = get(&app, "/api/auth/login/google", None).await;
        assert_eq!(resp.status, StatusCode::TEMPORARY_REDIRECT);
        let location = resp.headers[axum::http::header::LOCATION].to_str().unwrap();
        assert!(location.starts_with("https://accounts.google.com/"));
        assert!(location.contains("state="));
        assert!(location.contains("code_challenge="));
        assert!(location.contains("code_challenge_method=S256"));

        let set_cookie = resp.headers[axum::http::header::SET_COOKIE]
            .to_str()
            .unwrap();
        assert!(set_cookie.starts_with(&format!("{OAUTH_FLOW_COOKIE}=")));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("Secure"));
        assert!(set_cookie.contains("SameSite=Lax"));
        assert!(set_cookie.contains(&format!("Path={OAUTH_CALLBACK_PATH}")));
        assert!(
            !set_cookie.contains("pkce_verifier"),
            "flow cookie must be encrypted"
        );
    }

    #[tokio::test]
    async fn login_unknown_or_unconfigured_provider_is_404() {
        let app = build_app(oauth_state());
        for uri in ["/api/auth/login/github", "/api/auth/login/myspace"] {
            assert_eq!(
                get(&app, uri, None).await.status,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn callback_rejects_state_mismatch() {
        let state = oauth_state();
        let app = build_app(state.clone());
        let cookie = flow_cookie(&state, "google", "expected-state");
        let resp = get(
            &app,
            "/api/auth/callback/google?code=abc&state=attacker-state",
            Some(&cookie),
        )
        .await;
        assert_eq!(resp.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn callback_rejects_missing_state_or_cookie() {
        let state = oauth_state();
        let app = build_app(state.clone());
        let cookie = flow_cookie(&state, "google", "expected-state");
        let cases = [
            ("/api/auth/callback/google?code=abc", Some(cookie.as_str())),
            (
                "/api/auth/callback/google?code=abc&state=expected-state",
                None,
            ),
            (
                "/api/auth/callback/google?code=abc&state=expected-state",
                Some("unlinked_oauth=forged"),
            ),
        ];
        for (uri, cookie) in cases {
            let resp = get(&app, uri, cookie).await;
            assert_eq!(resp.status, StatusCode::FORBIDDEN, "{uri} {cookie:?}");
        }
    }

    #[tokio::test]
    async fn callback_rejects_flow_started_for_another_provider() {
        let state = oauth_state();
        let app = build_app(state.clone());
        let cookie = flow_cookie(&state, "github", "s");
        let resp = get(
            &app,
            "/api/auth/callback/google?code=abc&state=s",
            Some(&cookie),
        )
        .await;
        assert_eq!(resp.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn dev_login_is_404_outside_dev_mode() {
        let app = build_app(offline_state(test_config()));
        let resp = get(&app, "/api/auth/dev-login", None).await;
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn logout_without_session_succeeds() {
        let app = build_app(offline_state(test_config()));
        let resp = send(&app, Method::POST, "/api/auth/logout", &[], Body::empty()).await;
        assert_eq!(resp.status, StatusCode::NO_CONTENT);
    }
}
