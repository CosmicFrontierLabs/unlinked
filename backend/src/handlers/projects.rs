//! Projects and their explicit members.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use diesel::prelude::*;
use diesel::PgConnection;
use shared::{
    AddProjectMemberRequest, CreateProjectRequest, DefaultProjectRole, OrgRole, ProjectRole,
    UpdateProjectMemberRequest, UpdateProjectRequest,
};
use uuid::Uuid;

use crate::access::{visible_projects, OrgAccess, ProjectAccess};
use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::handlers::orgs::{user_by_email, validate_name};
use crate::models::{
    parse_role, NewProject, NewProjectMember, Project, ProjectChanges, ProjectMember, User,
};
use crate::schema::{project_members, projects, users};
use crate::session::CurrentUser;
use crate::AppState;

const MAX_DESCRIPTION_CHARS: usize = 10_000;

fn validate_description(description: &str) -> ApiResult<()> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(ApiError::BadRequest(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    Ok(())
}

fn to_api_list(rows: Vec<(Project, ProjectRole)>) -> ApiResult<Json<Vec<shared::Project>>> {
    rows.iter()
        .map(|(p, role)| p.to_api(*role))
        .collect::<ApiResult<Vec<_>>>()
        .map(Json)
}

/// Every project the caller can see, across all organizations.
pub async fn list_all(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<Vec<shared::Project>>> {
    state
        .db(move |conn| to_api_list(visible_projects(conn, user.id, None)?))
        .await
}

/// The projects in one organization that the caller can see.
pub async fn list_in_org(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
) -> ApiResult<Json<Vec<shared::Project>>> {
    state
        .db(move |conn| to_api_list(visible_projects(conn, access.user.id, Some(access.org.id))?))
        .await
}

/// Create a project in an organization. Any org member may; the creator
/// becomes the project's owner.
pub async fn create(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Json(req): Json<CreateProjectRequest>,
) -> ApiResult<(StatusCode, Json<shared::Project>)> {
    access.require(OrgRole::Member)?;
    let name = validate_name(&req.name)?.to_string();
    validate_description(&req.description)?;
    let default_role = req.default_role.unwrap_or(DefaultProjectRole::Viewer);
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let project: Project = diesel::insert_into(projects::table)
                    .values(NewProject {
                        org_id: access.org.id,
                        name: &name,
                        description: &req.description,
                        default_role: default_role.as_str(),
                        created_by: Some(access.user.id),
                    })
                    .returning(Project::as_returning())
                    .get_result(conn)?;
                diesel::insert_into(project_members::table)
                    .values(NewProjectMember {
                        project_id: project.id,
                        user_id: access.user.id,
                        role: ProjectRole::Owner.as_str(),
                    })
                    .execute(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(project.org_id),
                    Some(project.id),
                    "project.create",
                    serde_json::json!({ "name": name }),
                )?;
                Ok((
                    StatusCode::CREATED,
                    Json(project.to_api(ProjectRole::Owner)?),
                ))
            })
        })
        .await
}

pub async fn get(access: ProjectAccess) -> ApiResult<Json<shared::Project>> {
    access.project.to_api(access.role).map(Json)
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Json(req): Json<UpdateProjectRequest>,
) -> ApiResult<Json<shared::Project>> {
    access.require(ProjectRole::Owner)?;
    let name = req
        .name
        .as_deref()
        .map(validate_name)
        .transpose()?
        .map(str::to_string);
    if let Some(description) = &req.description {
        validate_description(description)?;
    }
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let id = access.project.id;
                let changes = ProjectChanges {
                    name: name.as_deref(),
                    description: req.description.as_deref(),
                    default_role: req.default_role.map(DefaultProjectRole::as_str),
                };
                let project: Project = if changes.is_empty() {
                    access.project.clone()
                } else {
                    diesel::update(projects::table.find(id))
                        .set(&changes)
                        .returning(Project::as_returning())
                        .get_result(conn)?
                };
                audit::record(
                    conn,
                    access.user.id,
                    Some(project.org_id),
                    Some(id),
                    "project.update",
                    serde_json::json!({
                        "name": name,
                        "description_changed": req.description.is_some(),
                        "default_role": req.default_role,
                    }),
                )?;
                Ok(Json(project.to_api(access.role)?))
            })
        })
        .await
}

/// Delete a project with all its files and history. Project owners only.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
) -> ApiResult<StatusCode> {
    access.require(ProjectRole::Owner)?;
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                diesel::delete(projects::table.find(access.project.id)).execute(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    None,
                    "project.delete",
                    serde_json::json!({
                        "project_id": access.project.id,
                        "name": access.project.name,
                    }),
                )?;
                Ok(StatusCode::NO_CONTENT)
            })
        })
        .await
}

fn member_rows(conn: &mut PgConnection, project_id: Uuid) -> ApiResult<Vec<shared::ProjectMember>> {
    project_members::table
        .inner_join(users::table)
        .filter(project_members::project_id.eq(project_id))
        .order(users::email)
        .select((ProjectMember::as_select(), User::as_select()))
        .load::<(ProjectMember, User)>(conn)?
        .into_iter()
        .map(|(m, u)| {
            Ok(shared::ProjectMember {
                user: u.into(),
                role: parse_role(&m.role, ProjectRole::parse)?,
                added_at: m.created_at,
            })
        })
        .collect()
}

pub async fn members(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
) -> ApiResult<Json<Vec<shared::ProjectMember>>> {
    state
        .db(move |conn| member_rows(conn, access.project.id).map(Json))
        .await
}

pub async fn add_member(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Json(req): Json<AddProjectMemberRequest>,
) -> ApiResult<(StatusCode, Json<Vec<shared::ProjectMember>>)> {
    access.require(ProjectRole::Owner)?;
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let invitee = user_by_email(conn, &req.email)?;
                let inserted = diesel::insert_into(project_members::table)
                    .values(NewProjectMember {
                        project_id: access.project.id,
                        user_id: invitee.id,
                        role: req.role.as_str(),
                    })
                    .on_conflict_do_nothing()
                    .execute(conn)?;
                if inserted == 0 {
                    return Err(ApiError::Conflict("already a member".to_string()));
                }
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(access.project.id),
                    "project.member.add",
                    serde_json::json!({ "user_id": invitee.id, "role": req.role }),
                )?;
                Ok((
                    StatusCode::CREATED,
                    Json(member_rows(conn, access.project.id)?),
                ))
            })
        })
        .await
}

pub async fn update_member(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateProjectMemberRequest>,
) -> ApiResult<Json<Vec<shared::ProjectMember>>> {
    access.require(ProjectRole::Owner)?;
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let updated =
                    diesel::update(project_members::table.find((access.project.id, user_id)))
                        .set(project_members::role.eq(req.role.as_str()))
                        .execute(conn)?;
                if updated == 0 {
                    return Err(ApiError::NotFound);
                }
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(access.project.id),
                    "project.member.role",
                    serde_json::json!({ "user_id": user_id, "role": req.role }),
                )?;
                Ok(Json(member_rows(conn, access.project.id)?))
            })
        })
        .await
}

/// Remove an explicit member (or leave, when `user_id` is the caller).
pub async fn remove_member(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    if user_id != access.user.id {
        access.require(ProjectRole::Owner)?;
    }
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let removed =
                    diesel::delete(project_members::table.find((access.project.id, user_id)))
                        .execute(conn)?;
                if removed == 0 {
                    return Err(ApiError::NotFound);
                }
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(access.project.id),
                    "project.member.remove",
                    serde_json::json!({ "user_id": user_id }),
                )?;
                Ok(StatusCode::NO_CONTENT)
            })
        })
        .await
}
