//! Organizations, their members, and their audit log.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use diesel::prelude::*;
use diesel::PgConnection;
use serde::Deserialize;
use shared::{
    AddOrgMemberRequest, AuditEntry, CreateOrgRequest, OrgRole, UpdateOrgMemberRequest,
    UpdateOrgRequest,
};
use uuid::Uuid;

use crate::access::OrgAccess;
use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::models::{
    parse_role, AuditRow, NewOrgMember, NewOrganization, OrgMember, Organization, User,
};
use crate::schema::{audit_log, org_members, organizations, project_members, projects, users};
use crate::session::CurrentUser;
use crate::AppState;

const MAX_AUDIT_ENTRIES: i64 = 1000;
const DEFAULT_AUDIT_ENTRIES: i64 = 100;

pub fn validate_name(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::BadRequest(
            "name must be 1-200 characters".to_string(),
        ));
    }
    Ok(name)
}

/// Look up an existing user by email for invitations.
pub fn user_by_email(conn: &mut PgConnection, email: &str) -> ApiResult<User> {
    users::table
        .filter(users::email.eq(email.trim().to_lowercase()))
        .select(User::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| {
            ApiError::BadRequest("no user with that email has signed in yet".to_string())
        })
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<Json<Vec<shared::Organization>>> {
    state
        .db(move |conn| {
            organizations::table
                .inner_join(org_members::table)
                .filter(org_members::user_id.eq(user.id))
                .order((organizations::name, organizations::id))
                .select((Organization::as_select(), org_members::role))
                .load::<(Organization, String)>(conn)?
                .into_iter()
                .map(|(org, role)| Ok(org.to_api(parse_role(&role, OrgRole::parse)?)))
                .collect::<ApiResult<Vec<_>>>()
                .map(Json)
        })
        .await
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<CreateOrgRequest>,
) -> ApiResult<(StatusCode, Json<shared::Organization>)> {
    let name = validate_name(&req.name)?.to_string();
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let org: Organization = diesel::insert_into(organizations::table)
                    .values(NewOrganization {
                        name: &name,
                        created_by: Some(user.id),
                    })
                    .returning(Organization::as_returning())
                    .get_result(conn)?;
                diesel::insert_into(org_members::table)
                    .values(NewOrgMember {
                        org_id: org.id,
                        user_id: user.id,
                        role: OrgRole::Owner.as_str(),
                    })
                    .execute(conn)?;
                audit::record(
                    conn,
                    user.id,
                    Some(org.id),
                    None,
                    "org.create",
                    serde_json::json!({ "name": name }),
                )?;
                Ok((StatusCode::CREATED, Json(org.to_api(OrgRole::Owner))))
            })
        })
        .await
}

pub async fn get(access: OrgAccess) -> Json<shared::Organization> {
    Json(access.org.to_api(access.role))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Json(req): Json<UpdateOrgRequest>,
) -> ApiResult<Json<shared::Organization>> {
    access.require(OrgRole::Admin)?;
    let name = validate_name(&req.name)?.to_string();
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let org: Organization = diesel::update(organizations::table.find(access.org.id))
                    .set(organizations::name.eq(&name))
                    .returning(Organization::as_returning())
                    .get_result(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(org.id),
                    None,
                    "org.rename",
                    serde_json::json!({ "name": name }),
                )?;
                Ok(Json(org.to_api(access.role)))
            })
        })
        .await
}

/// Delete an organization with all its projects and files. Owners only.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
) -> ApiResult<StatusCode> {
    access.require(OrgRole::Owner)?;
    state
        .db(move |conn| {
            diesel::delete(organizations::table.find(access.org.id)).execute(conn)?;
            tracing::info!(org_id = %access.org.id, user_id = %access.user.id, "organization deleted");
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

fn member_rows(conn: &mut PgConnection, org_id: Uuid) -> ApiResult<Vec<shared::OrgMember>> {
    org_members::table
        .inner_join(users::table)
        .filter(org_members::org_id.eq(org_id))
        .order(users::email)
        .select((OrgMember::as_select(), User::as_select()))
        .load::<(OrgMember, User)>(conn)?
        .into_iter()
        .map(|(m, u)| {
            Ok(shared::OrgMember {
                user: u.into(),
                role: parse_role(&m.role, OrgRole::parse)?,
                joined_at: m.created_at,
            })
        })
        .collect()
}

pub async fn members(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
) -> ApiResult<Json<Vec<shared::OrgMember>>> {
    state
        .db(move |conn| member_rows(conn, access.org.id).map(Json))
        .await
}

/// Lock the organization row so concurrent membership changes serialize and
/// the "at least one owner" invariant cannot be raced.
fn lock_org(conn: &mut PgConnection, org_id: Uuid) -> ApiResult<()> {
    organizations::table
        .find(org_id)
        .select(organizations::id)
        .for_update()
        .first::<Uuid>(conn)?;
    Ok(())
}

fn owner_count(conn: &mut PgConnection, org_id: Uuid) -> ApiResult<i64> {
    Ok(org_members::table
        .filter(org_members::org_id.eq(org_id))
        .filter(org_members::role.eq(OrgRole::Owner.as_str()))
        .count()
        .get_result(conn)?)
}

fn member_role(conn: &mut PgConnection, org_id: Uuid, user_id: Uuid) -> ApiResult<OrgRole> {
    let role: String = org_members::table
        .find((org_id, user_id))
        .select(org_members::role)
        .first(conn)?;
    parse_role(&role, OrgRole::parse)
}

pub async fn add_member(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Json(req): Json<AddOrgMemberRequest>,
) -> ApiResult<(StatusCode, Json<Vec<shared::OrgMember>>)> {
    access.require(OrgRole::Admin)?;
    if req.role == OrgRole::Owner {
        access.require(OrgRole::Owner)?;
    }
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let invitee = user_by_email(conn, &req.email)?;
                let inserted = diesel::insert_into(org_members::table)
                    .values(NewOrgMember {
                        org_id: access.org.id,
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
                    Some(access.org.id),
                    None,
                    "org.member.add",
                    serde_json::json!({ "user_id": invitee.id, "role": req.role }),
                )?;
                Ok((StatusCode::CREATED, Json(member_rows(conn, access.org.id)?)))
            })
        })
        .await
}

/// Change a member's role. Admins manage members and admins; only owners may
/// grant or revoke ownership, and the last owner cannot be demoted.
pub async fn update_member(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Path((_, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateOrgMemberRequest>,
) -> ApiResult<Json<Vec<shared::OrgMember>>> {
    access.require(OrgRole::Admin)?;
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let org_id = access.org.id;
                lock_org(conn, org_id)?;
                let current = member_role(conn, org_id, user_id)?;
                if current == OrgRole::Owner || req.role == OrgRole::Owner {
                    access.require(OrgRole::Owner)?;
                }
                if current == OrgRole::Owner
                    && req.role != OrgRole::Owner
                    && owner_count(conn, org_id)? <= 1
                {
                    return Err(ApiError::Conflict(
                        "an organization must keep at least one owner".to_string(),
                    ));
                }
                diesel::update(org_members::table.find((org_id, user_id)))
                    .set(org_members::role.eq(req.role.as_str()))
                    .execute(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(org_id),
                    None,
                    "org.member.role",
                    serde_json::json!({ "user_id": user_id, "role": req.role }),
                )?;
                Ok(Json(member_rows(conn, org_id)?))
            })
        })
        .await
}

/// Remove a member (or leave, when `user_id` is the caller). Their explicit
/// memberships on the organization's projects go with them.
pub async fn remove_member(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Path((_, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    if user_id != access.user.id {
        access.require(OrgRole::Admin)?;
    }
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let org_id = access.org.id;
                lock_org(conn, org_id)?;
                let current = member_role(conn, org_id, user_id)?;
                if current == OrgRole::Owner {
                    if user_id != access.user.id {
                        access.require(OrgRole::Owner)?;
                    }
                    if owner_count(conn, org_id)? <= 1 {
                        return Err(ApiError::Conflict(
                            "an organization must keep at least one owner".to_string(),
                        ));
                    }
                }
                diesel::delete(org_members::table.find((org_id, user_id))).execute(conn)?;
                let org_projects = projects::table
                    .filter(projects::org_id.eq(org_id))
                    .select(projects::id);
                diesel::delete(
                    project_members::table
                        .filter(project_members::user_id.eq(user_id))
                        .filter(project_members::project_id.eq_any(org_projects)),
                )
                .execute(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(org_id),
                    None,
                    "org.member.remove",
                    serde_json::json!({ "user_id": user_id }),
                )?;
                Ok(StatusCode::NO_CONTENT)
            })
        })
        .await
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    limit: Option<i64>,
}

/// Recent audit entries for an organization, newest first. Admins only.
pub async fn audit_log(
    State(state): State<Arc<AppState>>,
    access: OrgAccess,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    access.require(OrgRole::Admin)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_AUDIT_ENTRIES)
        .clamp(1, MAX_AUDIT_ENTRIES);
    state
        .db(move |conn| {
            let rows = audit_log::table
                .left_join(users::table)
                .filter(audit_log::org_id.eq(access.org.id))
                .order((audit_log::created_at.desc(), audit_log::id))
                .limit(limit)
                .select((AuditRow::as_select(), Option::<User>::as_select()))
                .load::<(AuditRow, Option<User>)>(conn)?;
            Ok(Json(
                rows.into_iter()
                    .map(|(row, actor)| AuditEntry {
                        id: row.id,
                        actor: actor.map(Into::into),
                        project_id: row.project_id,
                        action: row.action,
                        detail: row.detail,
                        created_at: row.created_at,
                    })
                    .collect(),
            ))
        })
        .await
}
