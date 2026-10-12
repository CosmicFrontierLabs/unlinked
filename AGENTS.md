# CLAUDE.md

## Crate Recommendations

### Static Asset Serving (Web Projects)

Use **`memory-serve`** for embedding and serving static frontend assets in axum web servers.

- Pre-compresses assets (brotli/gzip) at build time, zero CPU at startup
- Built-in content negotiation, ETag/304, cache-control headers, SPA fallback
- Replaces `rust-embed` + manual compression entirely

**Setup:**

```toml
[dependencies]
memory-serve = "2.1"

[build-dependencies]
memory-serve = "2.1"
```

```rust
// build.rs
fn main() {
    memory_serve::load_directory("./frontend/dist");
}
```

```rust
// main.rs
use memory_serve::CacheControl;

let frontend = memory_serve::load!()
    .index_file(Some("/index.html"))
    .fallback(Some("/index.html"))
    .fallback_status(axum::http::StatusCode::OK)
    .html_cache_control(CacheControl::NoCache)
    .cache_control(CacheControl::Long)
    .into_router();

let app = Router::new()
    // API routes first
    .route("/api/health", get(|| async { "ok" }))
    .with_state(app_state)
    .merge(frontend);
```

Note: memory-serve 2.x requires axum 0.8+. For axum 0.7, use memory-serve 0.6.0 (older `load_assets!` macro API).

## Backend Patterns

- **Authorization goes through one place.** Handlers take `session::CurrentUser`
  (any signed-in user), `access::OrgAccess` (`:org_id` path segment) or
  `access::ProjectAccess` (`:project_id`), then call `.require(min_role)`.
  No role on a resource answers 404, never 403; `require` failing answers 403.
  Effective project roles come only from `access::effective_project_role`
  (org owner/admin => Owner, else explicit membership, else org default role).
  Scope every child-resource query (files, versions, members) by the parent id
  from the access struct.
- **Blocking DB work** runs via `state.db(move |conn| ...)` (spawn_blocking
  with a pooled connection); errors are `error::ApiError` (diesel `NotFound`
  => 404, unique violation => 409).
- **CSRF:** `origin::require_same_origin` rejects unsafe methods whose
  `Origin`/`Referer` is not `PUBLIC_URL` (dev mode also allows localhost).
  There is deliberately no CORS layer. WebSocket routes must authenticate with
  `session::WsUser` or the `session::require_ws_user` route layer, which also
  require a same-origin `Origin`.
- **Unknown `/api/*` paths answer a JSON 404** (`api_not_found` in `main.rs`);
  only non-API paths fall back to the SPA's `index.html`.
- **Mutations write an audit entry** with `audit::record` inside the same
  transaction.
- **Tests:** `test_support` builds states/clients. DB-backed tests start with
  `let Some((state, app)) = db_app() else { return };` so they skip without
  `DATABASE_URL`. Run them locally with
  `docker run -d --name unlinked-pg-test -e POSTGRES_PASSWORD=pw -e POSTGRES_USER=unlinked -e POSTGRES_DB=unlinked -p 55432:5432 postgres:16-alpine`
  and `DATABASE_URL=postgres://unlinked:pw@localhost:55432/unlinked cargo test -p backend`.
- Regenerate `backend/src/schema.rs` with `diesel print-schema` after adding a
  migration; name migrations `YYYY-MM-DD-HHMMSS_snake_case`.

## Workspace and Simulation UI Patterns

- `FilePage` keeps the model editor and simulation panel mounted across tabs.
  Pass `active` to suppress hidden keyboard handlers and defer layout work.
  Metadata refreshes must not replace these panes: a new model version is
  opened explicitly so unsaved edits and active runs survive rename/upload.
- Navigation guards live in `navigation.rs` and `FilePage`; keep link, browser
  history, logout, and unload paths consistent. Fragment-only focus navigation
  must not prompt to discard a workspace.
- `sim::Controller` owns streamed samples, the plot session, and a generation
  counter. Every asynchronous completion checks its generation before writing
  state. Run initiation also uses a synchronous in-flight guard: Yew state
  updates alone cannot prevent two submissions in the same browser turn.
- Published simulation actions dispatch through current callback references,
  so the diagram uses the current form without capturing stale field values.
  A stopped run must stay stopping even if Started arrives after Stop.
- Keep raw numeric form drafts until validation succeeds. Historical result
  loading does not change the next run's setup; copying settings is explicit.
  Initialization-script selection distinguishes file ID and immutable version.
- Preserve plot limits across visibility changes and coalesce resize events.
  Reference runs keep independent time grids and match signals by recorded ID.
  Cursor measurements use recorded samples, not interpolated values.
- `frontend/browser-tests/` contains Playwright checks against a dev server.
  Use `--url`, `--model /projects/<id>/files/<id>`, and optional
  `--browser` executable path. Select a browser with
  `--engine chromium|firefox|webkit` (default Chromium)
  (the project-browser check instead takes `--project /projects/<id>`).
  Seed a model with a completed simulation first; the parameter check expects
  a Step block first, as in `digital_pi_first_order`. These checks mock writes
  and simulation streams and leave model edits unsaved.
