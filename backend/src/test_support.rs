//! Test-only helpers: canonical config/state, a process-wide Postgres pool for
//! DB-backed tests, and in-process request helpers.
//!
//! DB-backed tests call [`shared_pool`] and return early when it yields `None`
//! (no `DATABASE_URL`), so `cargo test` stays green without Postgres. With it
//! set, migrations run once and every test creates its own uniquely named
//! users, so tests never interfere with each other or need cleanup.

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::http::{header, HeaderName, Method, Request, StatusCode};
use axum::Router;
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tower::ServiceExt;
use tower_cookies::cookie::{Cookie, CookieJar};
use tower_cookies::Key;
use uuid::Uuid;

use crate::config::Config;
use crate::db::{run_migrations, DbPool};
use crate::models::User;
use crate::oauth::OAuthProviders;
use crate::session::{create_session, SESSION_COOKIE};
use crate::{build_app, AppState};

/// Upload cap used by tests, small enough to exceed cheaply.
pub const TEST_MAX_UPLOAD_BYTES: usize = 64 * 1024;
pub const TEST_PUBLIC_URL: &str = "https://unlinked.example.com";

pub fn test_config() -> Config {
    Config {
        host: "127.0.0.1".to_string(),
        port: 0,
        public_url: TEST_PUBLIC_URL.parse().unwrap(),
        cookie_key: Key::generate(),
        oauth: OAuthProviders::default(),
        allowed_email_domains: Vec::new(),
        max_upload_bytes: TEST_MAX_UPLOAD_BYTES,
        session_ttl: chrono::Duration::days(1),
    }
}

/// State whose pool never connects: for routes that must answer before
/// touching the database (401s, CSRF rejection, static assets).
pub fn offline_state(config: Config) -> Arc<AppState> {
    let manager = ConnectionManager::<PgConnection>::new("postgres://invalid/db");
    Arc::new(AppState {
        dev_mode: false,
        db_pool: Pool::builder().build_unchecked(manager),
        config,
    })
}

/// The process-wide test pool, or `None` when `DATABASE_URL` is unset.
pub fn shared_pool() -> Option<DbPool> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set, skipping DB-backed test");
        return None;
    };
    static POOL: OnceLock<DbPool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let pool = Pool::builder()
            .max_size(16)
            .build(ConnectionManager::<PgConnection>::new(url))
            .expect("connect to DATABASE_URL");
        run_migrations(&pool).expect("run migrations");
        pool
    });
    Some(pool.clone())
}

pub fn db_state(pool: DbPool) -> Arc<AppState> {
    Arc::new(AppState {
        dev_mode: false,
        db_pool: pool,
        config: test_config(),
    })
}

/// A `Cookie` header value carrying `value` as an encrypted cookie that the
/// server will accept.
pub fn private_cookie_header(key: &Key, name: &str, value: &str) -> String {
    let mut jar = CookieJar::new();
    jar.private_mut(key)
        .add(Cookie::new(name.to_string(), value.to_string()));
    let cookie = jar.get(name).expect("cookie just added");
    format!("{}={}", cookie.name(), cookie.value())
}

/// A signed-in user and the `Cookie` header for their session.
pub struct TestUser {
    pub user: User,
    pub cookie: String,
}

impl TestUser {
    pub fn email(&self) -> &str {
        &self.user.email
    }
}

/// Create a fresh user with a live session.
pub fn sign_up(state: &AppState, label: &str) -> TestUser {
    let mut conn = state.db_pool.get().expect("test connection");
    let email = format!("{label}-{}@example.com", Uuid::new_v4());
    let subject = Uuid::new_v4().to_string();
    let user = crate::handlers::auth::resolve_user(
        &mut conn,
        ("test", &subject),
        &email,
        Some(label),
        None,
    )
    .expect("create user");
    let token = create_session(&mut conn, user.id, chrono::Duration::hours(1)).expect("session");
    TestUser {
        user,
        cookie: private_cookie_header(&state.config.cookie_key, SESSION_COOKIE, &token),
    }
}

/// A response's status and raw body.
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "invalid JSON ({e}) with status {}: {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

/// Drive `app` in-process with one request.
pub async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    headers: &[(HeaderName, String)],
    body: Body,
) -> TestResponse {
    let mut req = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        req = req.header(name, value);
    }
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    TestResponse {
        status,
        headers,
        body,
    }
}

/// An in-process client acting as one user from the app's own origin, like
/// the embedded frontend would.
pub struct Client<'a> {
    pub app: &'a Router,
    pub headers: Vec<(HeaderName, String)>,
}

impl<'a> Client<'a> {
    pub fn new(app: &'a Router, user: &TestUser) -> Self {
        Self {
            app,
            headers: vec![
                (header::COOKIE, user.cookie.clone()),
                (header::ORIGIN, TEST_PUBLIC_URL.to_string()),
            ],
        }
    }

    /// The same user, but with `name` set to `value` (or removed when `None`).
    pub fn with_header(mut self, name: HeaderName, value: Option<&str>) -> Self {
        self.headers.retain(|(n, _)| *n != name);
        if let Some(value) = value {
            self.headers.push((name, value.to_string()));
        }
        self
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Body,
        content_type: Option<&str>,
    ) -> TestResponse {
        let mut headers = self.headers.clone();
        if let Some(ct) = content_type {
            headers.push((header::CONTENT_TYPE, ct.to_string()));
        }
        send(self.app, method, uri, &headers, body).await
    }

    pub async fn get(&self, uri: &str) -> TestResponse {
        self.send(Method::GET, uri, Body::empty(), None).await
    }

    pub async fn delete(&self, uri: &str) -> TestResponse {
        self.send(Method::DELETE, uri, Body::empty(), None).await
    }

    pub async fn json<B: Serialize>(&self, method: Method, uri: &str, body: &B) -> TestResponse {
        self.send(
            method,
            uri,
            Body::from(serde_json::to_vec(body).unwrap()),
            Some("application/json"),
        )
        .await
    }

    pub async fn upload(&self, project_id: Uuid, path: &str, bytes: Vec<u8>) -> TestResponse {
        let uri = format!(
            "/api/projects/{project_id}/files?path={}&message=test",
            urlencode(path)
        );
        self.send(
            Method::POST,
            &uri,
            Body::from(bytes),
            Some("application/octet-stream"),
        )
        .await
    }
}

/// Minimal percent-encoding for query values in tests.
pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Build the app for a DB-backed test, or `None` to skip.
pub fn db_app() -> Option<(Arc<AppState>, Router)> {
    let state = db_state(shared_pool()?);
    let app = build_app(state.clone());
    Some((state, app))
}
