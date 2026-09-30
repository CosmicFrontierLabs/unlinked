//! Typed HTTP client for the backend API. Requests are same-origin, so the
//! browser attaches the session cookie and `Origin` header the server's
//! CSRF check expects.

use gloo_net::http::{Request, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;
use shared::{
    AddOrgMemberRequest, AddProjectMemberRequest, AuditEntry, AuthProvidersResponse,
    CreateOrgRequest, CreateProjectRequest, ErrorResponse, FileInfo, FileVersionInfo, OrgMember,
    OrgRole, Organization, Project, ProjectMember, ProjectRole, RenameFileRequest,
    SimulationResult, SimulationRun, UpdateOrgMemberRequest, UpdateProjectMemberRequest, UserInfo,
};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} ({})", self.message, self.status)
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

fn network(e: impl fmt::Display) -> ApiError {
    ApiError {
        status: 0,
        message: e.to_string(),
    }
}

async fn check(resp: Response) -> ApiResult<Response> {
    if resp.ok() {
        return Ok(resp);
    }
    let status = resp.status();
    let message = match resp.json::<ErrorResponse>().await {
        Ok(e) => e.error,
        Err(_) => resp.status_text(),
    };
    Err(ApiError { status, message })
}

async fn send(req: RequestBuilder) -> ApiResult<Response> {
    check(req.send().await.map_err(network)?).await
}

async fn json<T: DeserializeOwned>(req: RequestBuilder) -> ApiResult<T> {
    send(req).await?.json().await.map_err(network)
}

async fn with_body<B: Serialize, T: DeserializeOwned>(
    req: RequestBuilder,
    body: &B,
) -> ApiResult<T> {
    let resp = req
        .json(body)
        .map_err(network)?
        .send()
        .await
        .map_err(network)?;
    check(resp).await?.json().await.map_err(network)
}

async fn with_body_empty<B: Serialize>(req: RequestBuilder, body: &B) -> ApiResult<()> {
    let resp = req
        .json(body)
        .map_err(network)?
        .send()
        .await
        .map_err(network)?;
    check(resp).await.map(|_| ())
}

fn encode(s: &str) -> String {
    js_sys::encode_uri_component(s).into()
}

// Auth

pub async fn providers() -> ApiResult<AuthProvidersResponse> {
    json(Request::get("/api/auth/providers")).await
}

/// The signed-in user, or `None` when not signed in.
pub async fn me() -> ApiResult<Option<UserInfo>> {
    match json(Request::get("/api/me")).await {
        Ok(u) => Ok(Some(u)),
        Err(e) if e.status == 401 => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn logout() -> ApiResult<()> {
    send(Request::post("/api/auth/logout")).await.map(|_| ())
}

// Organizations

pub async fn orgs() -> ApiResult<Vec<Organization>> {
    json(Request::get("/api/orgs")).await
}

pub async fn org(id: Uuid) -> ApiResult<Organization> {
    json(Request::get(&format!("/api/orgs/{id}"))).await
}

pub async fn create_org(name: String) -> ApiResult<Organization> {
    with_body(Request::post("/api/orgs"), &CreateOrgRequest { name }).await
}

pub async fn org_members(id: Uuid) -> ApiResult<Vec<OrgMember>> {
    json(Request::get(&format!("/api/orgs/{id}/members"))).await
}

pub async fn add_org_member(id: Uuid, email: String, role: OrgRole) -> ApiResult<OrgMember> {
    with_body(
        Request::post(&format!("/api/orgs/{id}/members")),
        &AddOrgMemberRequest { email, role },
    )
    .await
}

pub async fn update_org_member(id: Uuid, user: Uuid, role: OrgRole) -> ApiResult<()> {
    with_body_empty(
        Request::patch(&format!("/api/orgs/{id}/members/{user}")),
        &UpdateOrgMemberRequest { role },
    )
    .await
}

pub async fn remove_org_member(id: Uuid, user: Uuid) -> ApiResult<()> {
    send(Request::delete(&format!("/api/orgs/{id}/members/{user}")))
        .await
        .map(|_| ())
}

pub async fn audit(id: Uuid) -> ApiResult<Vec<AuditEntry>> {
    json(Request::get(&format!("/api/orgs/{id}/audit"))).await
}

// Projects

pub async fn all_projects() -> ApiResult<Vec<Project>> {
    json(Request::get("/api/projects")).await
}

pub async fn org_projects(org: Uuid) -> ApiResult<Vec<Project>> {
    json(Request::get(&format!("/api/orgs/{org}/projects"))).await
}

pub async fn create_project(org: Uuid, req: CreateProjectRequest) -> ApiResult<Project> {
    with_body(Request::post(&format!("/api/orgs/{org}/projects")), &req).await
}

pub async fn project(id: Uuid) -> ApiResult<Project> {
    json(Request::get(&format!("/api/projects/{id}"))).await
}

pub async fn project_members(id: Uuid) -> ApiResult<Vec<ProjectMember>> {
    json(Request::get(&format!("/api/projects/{id}/members"))).await
}

pub async fn add_project_member(
    id: Uuid,
    email: String,
    role: ProjectRole,
) -> ApiResult<ProjectMember> {
    with_body(
        Request::post(&format!("/api/projects/{id}/members")),
        &AddProjectMemberRequest { email, role },
    )
    .await
}

pub async fn update_project_member(id: Uuid, user: Uuid, role: ProjectRole) -> ApiResult<()> {
    with_body_empty(
        Request::patch(&format!("/api/projects/{id}/members/{user}")),
        &UpdateProjectMemberRequest { role },
    )
    .await
}

pub async fn remove_project_member(id: Uuid, user: Uuid) -> ApiResult<()> {
    send(Request::delete(&format!(
        "/api/projects/{id}/members/{user}"
    )))
    .await
    .map(|_| ())
}

// Files

pub async fn files(project: Uuid) -> ApiResult<Vec<FileInfo>> {
    json(Request::get(&format!("/api/projects/{project}/files"))).await
}

pub async fn file(project: Uuid, file: Uuid) -> ApiResult<FileInfo> {
    json(Request::get(&format!(
        "/api/projects/{project}/files/{file}"
    )))
    .await
}

pub async fn versions(project: Uuid, file: Uuid) -> ApiResult<Vec<FileVersionInfo>> {
    json(Request::get(&format!(
        "/api/projects/{project}/files/{file}/versions"
    )))
    .await
}

/// Content of one version, or of the latest version when `version` is `None`.
pub async fn content(project: Uuid, file: Uuid, version: Option<Uuid>) -> ApiResult<Vec<u8>> {
    let url = match version {
        Some(v) => format!("/api/projects/{project}/files/{file}/versions/{v}/content"),
        None => format!("/api/projects/{project}/files/{file}/content"),
    };
    send(Request::get(&url))
        .await?
        .binary()
        .await
        .map_err(network)
}

/// The server's default `MAX_UPLOAD_BYTES`. Checked before reading a file
/// into memory; the server still enforces its configured limit.
const MAX_UPLOAD_BYTES: f64 = 50.0 * 1024.0 * 1024.0;

/// Read a user-selected file for upload, refusing oversized files up front.
pub async fn read_upload(file: web_sys::File) -> ApiResult<Vec<u8>> {
    let name = file.name();
    if file.size() > MAX_UPLOAD_BYTES {
        return Err(ApiError {
            status: 413,
            message: format!("{name} is larger than the 50 MiB upload limit"),
        });
    }
    gloo_file::futures::read_as_bytes(&gloo_file::File::from(file))
        .await
        .map_err(|e| network(format!("{name}: {e}")))
}

/// Store `bytes` as a new version of the file at `path` (creating it if new).
pub async fn upload(project: Uuid, path: &str, message: &str, bytes: &[u8]) -> ApiResult<FileInfo> {
    let url = format!(
        "/api/projects/{project}/files?path={}&message={}",
        encode(path),
        encode(message)
    );
    let body = js_sys::Uint8Array::from(bytes);
    let resp = Request::post(&url)
        .header("content-type", "application/octet-stream")
        .body(body)
        .map_err(network)?
        .send()
        .await
        .map_err(network)?;
    check(resp).await?.json().await.map_err(network)
}

// Simulations

pub async fn simulation_runs(file: Uuid) -> ApiResult<Vec<SimulationRun>> {
    json(Request::get(&format!("/api/files/{file}/simulations"))).await
}

pub async fn simulation_result(run: Uuid) -> ApiResult<SimulationResult> {
    json(Request::get(&format!("/api/simulations/{run}"))).await
}

pub async fn rename_file(project: Uuid, file: Uuid, path: String) -> ApiResult<()> {
    with_body_empty(
        Request::patch(&format!("/api/projects/{project}/files/{file}")),
        &RenameFileRequest { path },
    )
    .await
}

pub async fn delete_file(project: Uuid, file: Uuid) -> ApiResult<()> {
    send(Request::delete(&format!(
        "/api/projects/{project}/files/{file}"
    )))
    .await
    .map(|_| ())
}
