//! Authorization: the single place that decides what a user may see and do.
//!
//! Every handler touching an organization or project takes [`OrgAccess`] or
//! [`ProjectAccess`]. Both resolve the caller's role from the database and
//! answer 404 when the caller has no role at all, so the existence of other
//! tenants' resources is never revealed. Handlers then call `require` for the
//! minimum role an action needs, which answers 403 (the caller can already see
//! the resource, so 403 leaks nothing).

use std::collections::HashMap;
use std::sync::Arc;

use axum::async_trait;
use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::request::Parts;
use diesel::prelude::*;
use diesel::PgConnection;
use shared::{DefaultProjectRole, OrgRole, ProjectRole};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::models::{parse_role, Organization, Project, User};
use crate::schema::{org_members, organizations, project_members, projects};
use crate::session::CurrentUser;
use crate::AppState;

/// A user's effective role on a project.
///
/// Organization owners and admins always hold `Owner` so they can administer
/// every project in their organization. Otherwise an explicit project
/// membership wins (it may grant more *or less* than the default, letting a
/// project restrict an individual), and org members without one fall back to
/// the project's default role. Everyone else has no role.
pub fn effective_project_role(
    explicit: Option<ProjectRole>,
    org_role: Option<OrgRole>,
    default_role: DefaultProjectRole,
) -> Option<ProjectRole> {
    if matches!(org_role, Some(OrgRole::Owner | OrgRole::Admin)) {
        return Some(ProjectRole::Owner);
    }
    explicit.or_else(|| org_role.and_then(|_| default_role.project_role()))
}

pub fn org_role(
    conn: &mut PgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> ApiResult<Option<OrgRole>> {
    org_members::table
        .find((org_id, user_id))
        .select(org_members::role)
        .first::<String>(conn)
        .optional()?
        .map(|r| parse_role(&r, OrgRole::parse))
        .transpose()
}

pub fn project_role(
    conn: &mut PgConnection,
    user_id: Uuid,
    project: &Project,
) -> ApiResult<Option<ProjectRole>> {
    let explicit = project_members::table
        .find((project.id, user_id))
        .select(project_members::role)
        .first::<String>(conn)
        .optional()?
        .map(|r| parse_role(&r, ProjectRole::parse))
        .transpose()?;
    let org = org_role(conn, user_id, project.org_id)?;
    Ok(effective_project_role(
        explicit,
        org,
        project.default_role()?,
    ))
}

/// Every project `user_id` can see, with their role, optionally limited to
/// one organization. Uses [`effective_project_role`] so listings and
/// per-project checks can never disagree.
pub fn visible_projects(
    conn: &mut PgConnection,
    user_id: Uuid,
    org_filter: Option<Uuid>,
) -> ApiResult<Vec<(Project, ProjectRole)>> {
    let explicit: HashMap<Uuid, ProjectRole> = project_members::table
        .filter(project_members::user_id.eq(user_id))
        .select((project_members::project_id, project_members::role))
        .load::<(Uuid, String)>(conn)?
        .into_iter()
        .map(|(id, r)| Ok((id, parse_role(&r, ProjectRole::parse)?)))
        .collect::<ApiResult<_>>()?;
    let orgs: HashMap<Uuid, OrgRole> = org_members::table
        .filter(org_members::user_id.eq(user_id))
        .select((org_members::org_id, org_members::role))
        .load::<(Uuid, String)>(conn)?
        .into_iter()
        .map(|(id, r)| Ok((id, parse_role(&r, OrgRole::parse)?)))
        .collect::<ApiResult<_>>()?;

    let explicit_ids: Vec<Uuid> = explicit.keys().copied().collect();
    let org_ids: Vec<Uuid> = orgs.keys().copied().collect();
    let mut query = projects::table
        .filter(
            projects::id
                .eq_any(explicit_ids)
                .or(projects::org_id.eq_any(org_ids)),
        )
        .order((projects::name, projects::id))
        .select(Project::as_select())
        .into_boxed();
    if let Some(org_id) = org_filter {
        query = query.filter(projects::org_id.eq(org_id));
    }

    let mut out = Vec::new();
    for project in query.load(conn)? {
        let role = effective_project_role(
            explicit.get(&project.id).copied(),
            orgs.get(&project.org_id).copied(),
            project.default_role()?,
        );
        if let Some(role) = role {
            out.push((project, role));
        }
    }
    Ok(out)
}

/// Resolve the caller's access to one project; 404 if they have none.
pub fn resolve_project_access(
    conn: &mut PgConnection,
    user: User,
    project_id: Uuid,
) -> ApiResult<ProjectAccess> {
    let project = projects::table
        .find(project_id)
        .select(Project::as_select())
        .first(conn)
        .optional()?
        .ok_or(ApiError::NotFound)?;
    let role = project_role(conn, user.id, &project)?.ok_or(ApiError::NotFound)?;
    Ok(ProjectAccess {
        user,
        project,
        role,
    })
}

/// Resolve the caller's membership of one organization; 404 if none.
pub fn resolve_org_access(
    conn: &mut PgConnection,
    user: User,
    org_id: Uuid,
) -> ApiResult<OrgAccess> {
    let role = org_role(conn, user.id, org_id)?.ok_or(ApiError::NotFound)?;
    let org = organizations::table
        .find(org_id)
        .select(Organization::as_select())
        .first(conn)?;
    Ok(OrgAccess { user, org, role })
}

/// The caller, a project from the `:project_id` path segment, and the
/// caller's effective role on it.
pub struct ProjectAccess {
    pub user: User,
    pub project: Project,
    pub role: ProjectRole,
}

impl ProjectAccess {
    pub fn require(&self, min: ProjectRole) -> ApiResult<()> {
        if self.role >= min {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

/// The caller, an organization from the `:org_id` path segment, and the
/// caller's role in it.
pub struct OrgAccess {
    pub user: User,
    pub org: Organization,
    pub role: OrgRole,
}

impl OrgAccess {
    pub fn require(&self, min: OrgRole) -> ApiResult<()> {
        if self.role >= min {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

/// Parse a UUID path parameter by name; malformed or missing ids are 404.
async fn path_uuid(parts: &mut Parts, state: &Arc<AppState>, name: &str) -> ApiResult<Uuid> {
    let params = RawPathParams::from_request_parts(parts, state)
        .await
        .map_err(|_| ApiError::NotFound)?;
    params
        .iter()
        .find(|(k, _)| *k == name)
        .and_then(|(_, v)| Uuid::parse_str(v).ok())
        .ok_or(ApiError::NotFound)
}

#[async_trait]
impl FromRequestParts<Arc<AppState>> for ProjectAccess {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let project_id = path_uuid(parts, state, "project_id").await?;
        state
            .db(move |conn| resolve_project_access(conn, user, project_id))
            .await
    }
}

#[async_trait]
impl FromRequestParts<Arc<AppState>> for OrgAccess {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let org_id = path_uuid(parts, state, "org_id").await?;
        state
            .db(move |conn| resolve_org_access(conn, user, org_id))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DefaultProjectRole as D;
    use OrgRole as O;
    use ProjectRole as P;

    #[test]
    fn org_admins_and_owners_own_every_project() {
        for org in [O::Owner, O::Admin] {
            for explicit in [None, Some(P::Viewer)] {
                assert_eq!(
                    effective_project_role(explicit, Some(org), D::None),
                    Some(P::Owner)
                );
            }
        }
    }

    #[test]
    fn explicit_membership_overrides_default() {
        assert_eq!(
            effective_project_role(Some(P::Viewer), Some(O::Member), D::Editor),
            Some(P::Viewer)
        );
        assert_eq!(
            effective_project_role(Some(P::Editor), Some(O::Member), D::None),
            Some(P::Editor)
        );
    }

    #[test]
    fn org_members_get_default_role() {
        assert_eq!(
            effective_project_role(None, Some(O::Member), D::Editor),
            Some(P::Editor)
        );
        assert_eq!(effective_project_role(None, Some(O::Member), D::None), None);
    }

    #[test]
    fn outsiders_only_have_explicit_roles() {
        assert_eq!(effective_project_role(None, None, D::Editor), None);
        assert_eq!(
            effective_project_role(Some(P::Viewer), None, D::Editor),
            Some(P::Viewer)
        );
    }
}
