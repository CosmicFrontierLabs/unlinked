mod access;
#[cfg(test)]
mod api_tests;
mod audit;
mod config;
mod db;
mod error;
mod handlers;
mod models;
mod oauth;
mod origin;
mod schema;
mod session;
#[cfg(test)]
mod test_support;

use crate::config::Config;
use crate::db::DbPool;
use crate::error::{ApiError, ApiResult};
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Router};
use clap::Parser;
use diesel::PgConnection;
use handlers::{auth, files, orgs, projects};
use memory_serve::{load_assets, CacheControl, MemoryServe};
use std::sync::Arc;
use tower_cookies::CookieManagerLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use ws_bridge::WsEndpoint;

#[derive(Parser, Debug, Clone)]
#[command(name = "backend")]
#[command(about = "Backend server")]
struct Args {
    /// Enable development mode (relaxed config requirements)
    #[arg(long)]
    dev_mode: bool,
}

pub struct AppState {
    pub dev_mode: bool,
    pub db_pool: DbPool,
    pub config: Config,
}

impl AppState {
    /// Run blocking Diesel work on the blocking thread pool with a pooled
    /// connection, keeping the async executor free.
    pub async fn db<T, F>(&self, f: F) -> ApiResult<T>
    where
        F: FnOnce(&mut PgConnection) -> ApiResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.db_pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get()?;
            f(&mut conn)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("database task failed: {e}")))?
    }
}

/// All `/api` routes except health. Every handler authenticates through
/// `CurrentUser` or authorizes through `OrgAccess`/`ProjectAccess`.
fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auth/providers", get(auth::providers))
        .route("/api/auth/login/:provider", get(auth::login))
        .route("/api/auth/callback/:provider", get(auth::callback))
        .route("/api/auth/dev-login", get(auth::dev_login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/orgs", get(orgs::list).post(orgs::create))
        .route(
            "/api/orgs/:org_id",
            get(orgs::get).patch(orgs::update).delete(orgs::delete),
        )
        .route(
            "/api/orgs/:org_id/members",
            get(orgs::members).post(orgs::add_member),
        )
        .route(
            "/api/orgs/:org_id/members/:user_id",
            axum::routing::patch(orgs::update_member).delete(orgs::remove_member),
        )
        .route("/api/orgs/:org_id/audit", get(orgs::audit_log))
        .route(
            "/api/orgs/:org_id/projects",
            get(projects::list_in_org).post(projects::create),
        )
        .route("/api/projects", get(projects::list_all))
        .route(
            "/api/projects/:project_id",
            get(projects::get)
                .patch(projects::update)
                .delete(projects::delete),
        )
        .route(
            "/api/projects/:project_id/members",
            get(projects::members).post(projects::add_member),
        )
        .route(
            "/api/projects/:project_id/members/:user_id",
            axum::routing::patch(projects::update_member).delete(projects::remove_member),
        )
        .route(
            "/api/projects/:project_id/files",
            get(files::list).post(files::upload),
        )
        .route(
            "/api/projects/:project_id/files/:file_id",
            get(files::get).patch(files::rename).delete(files::delete),
        )
        .route(
            "/api/projects/:project_id/files/:file_id/content",
            get(files::latest_content),
        )
        .route(
            "/api/projects/:project_id/files/:file_id/versions",
            get(files::versions),
        )
        .route(
            "/api/projects/:project_id/files/:file_id/versions/:version_id/content",
            get(files::version_content),
        )
}

/// Build the full application router from shared state.
///
/// Kept as a pure function of `AppState` so tests can drive the entire app
/// in-process via `tower::ServiceExt::oneshot` — no bound port, no network.
///
/// No CORS layer: the frontend is served from this same origin, and
/// `origin::require_same_origin` rejects cross-origin state changes.
pub fn build_app(state: Arc<AppState>) -> Router {
    // Pre-compressed (brotli/gzip), content-negotiated frontend assets with
    // ETag/304 and an SPA fallback to index.html. Assets are embedded at build
    // time in release builds and read from disk in debug builds.
    let frontend = MemoryServe::new(load_assets!("../frontend/dist"))
        .index_file(Some("/index.html"))
        .fallback(Some("/index.html"))
        .fallback_status(StatusCode::OK)
        .html_cache_control(CacheControl::NoCache)
        .cache_control(CacheControl::Long)
        .into_router();

    // Both limits use MAX_UPLOAD_BYTES: the tower-http layer rejects oversized
    // Content-Length up front, DefaultBodyLimit caps buffered extractors
    // (`Bytes`, `Json`) for chunked bodies. Either answers 413.
    let body_limit = state.config.max_upload_bytes;

    Router::new()
        .route("/api/health", get(handlers::health::health))
        .merge(api_routes())
        .with_state(state.clone())
        .route(
            shared::AppSocket::PATH,
            handlers::websocket::handler().route_layer(middleware::from_fn_with_state(
                state.clone(),
                session::require_ws_user,
            )),
        )
        .merge(frontend)
        .layer(middleware::from_fn_with_state(
            state,
            origin::require_same_origin,
        ))
        .layer(CookieManagerLayer::new())
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(RequestBodyLimitLayer::new(body_limit))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Log panics with their source location and a backtrace via tracing, so
    // crashes are captured in structured logs rather than only on stderr.
    // Set RUST_BACKTRACE=1 to populate the backtrace.
    std::panic::set_hook(Box::new(|panic_info| {
        let backtrace = std::backtrace::Backtrace::capture();
        match panic_info.location() {
            Some(loc) => tracing::error!(
                "PANIC at {}:{}:{}: {}",
                loc.file(),
                loc.line(),
                loc.column(),
                panic_info
            ),
            None => tracing::error!("PANIC: {}", panic_info),
        }
        tracing::error!("Backtrace:\n{backtrace}");
    }));

    if args.dev_mode {
        tracing::warn!("DEV MODE ENABLED");
    }

    // Load .env file if present
    dotenvy::dotenv().ok();

    let config = Config::from_env(args.dev_mode)?;

    // Create database pool and run migrations
    let pool = db::create_pool()?;

    tracing::info!("Running database migrations...");
    match db::run_migrations(&pool) {
        Ok(applied) => {
            if applied.is_empty() {
                tracing::info!("Database is up to date");
            } else {
                for m in &applied {
                    tracing::info!("Applied migration: {}", m);
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to run migrations: {}", e);
            return Err(e);
        }
    }

    let bind_addr = config.bind_addr();
    let app_state = Arc::new(AppState {
        dev_mode: args.dev_mode,
        db_pool: pool,
        config,
    });

    let app = build_app(app_state);

    // Bind and serve
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down..."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{offline_state, test_config};
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use tower::ServiceExt;

    /// State with a pool that is never actually connected. The health and asset
    /// routes don't touch the database, so `build_unchecked` lets us exercise
    /// the whole router without a running Postgres.
    fn test_state() -> Arc<AppState> {
        offline_state(test_config())
    }

    #[tokio::test]
    async fn health_returns_ok_json() {
        let resp = build_app(test_state())
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("json"), "expected JSON, got {ct}");

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: shared::HealthResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.status, "ok");
    }

    #[tokio::test]
    async fn index_served_as_html() {
        let resp = build_app(test_state())
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("html"), "expected HTML, got {ct}");
    }

    #[tokio::test]
    async fn unknown_path_falls_back_to_index() {
        // SPA fallback: any unmatched path serves index.html with 200.
        let resp = build_app(test_state())
            .oneshot(
                Request::get("/some/client/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn websocket_requires_same_origin_and_session() {
        let cases = [
            (None, StatusCode::FORBIDDEN),
            (Some("https://evil.example.com"), StatusCode::FORBIDDEN),
            (
                Some(crate::test_support::TEST_PUBLIC_URL),
                StatusCode::UNAUTHORIZED,
            ),
        ];
        for (origin, expected) in cases {
            let mut req = Request::get(shared::AppSocket::PATH);
            if let Some(origin) = origin {
                req = req.header(header::ORIGIN, origin);
            }
            let resp = build_app(test_state())
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), expected, "{origin:?}");
        }
    }

    #[tokio::test]
    async fn unsafe_methods_are_checked_before_auth() {
        let session = "unlinked_session=anything";
        let public = crate::test_support::TEST_PUBLIC_URL;
        let cases: [(&[(header::HeaderName, &str)], StatusCode); 5] = [
            (
                &[(header::ORIGIN, "https://evil.example.com")],
                StatusCode::FORBIDDEN,
            ),
            (
                &[(header::REFERER, "https://evil.example.com/x")],
                StatusCode::FORBIDDEN,
            ),
            (&[(header::COOKIE, session)], StatusCode::FORBIDDEN),
            (&[(header::ORIGIN, public)], StatusCode::UNAUTHORIZED),
            (&[], StatusCode::UNAUTHORIZED),
        ];
        for (headers, expected) in cases {
            let mut req =
                Request::post("/api/orgs").header(header::CONTENT_TYPE, "application/json");
            for (name, value) in headers {
                req = req.header(name, *value);
            }
            let resp = build_app(test_state())
                .oneshot(req.body(Body::from("{\"name\":\"x\"}")).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), expected, "{headers:?}");
        }
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_before_auth() {
        let limit = crate::test_support::TEST_MAX_UPLOAD_BYTES;
        let resp = build_app(test_state())
            .oneshot(
                Request::post(format!(
                    "/api/projects/{}/files?path=big.bin",
                    uuid::Uuid::new_v4()
                ))
                .header(header::CONTENT_LENGTH, limit + 1)
                .body(Body::from(vec![0u8; limit + 1]))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
