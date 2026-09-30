use chrono::{DateTime, Utc};
use diesel::prelude::*;
use shared::{DefaultProjectRole, OrgRole, ProjectRole};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schema::{
    audit_log, file_versions, files, org_members, organizations, project_members, projects,
    sessions, user_identities, users,
};

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = users)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl From<User> for shared::UserInfo {
    fn from(u: User) -> Self {
        shared::UserInfo {
            id: u.id,
            email: u.email,
            name: u.name,
            avatar_url: u.avatar_url,
        }
    }
}

#[derive(Debug, Insertable)]
#[diesel(table_name = users)]
pub struct NewUser<'a> {
    pub email: &'a str,
    pub name: Option<&'a str>,
    pub avatar_url: Option<&'a str>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = user_identities)]
pub struct NewUserIdentity<'a> {
    pub user_id: Uuid,
    pub provider: &'a str,
    pub subject: &'a str,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = sessions)]
pub struct NewSession {
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = organizations)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

impl Organization {
    pub fn to_api(&self, my_role: OrgRole) -> shared::Organization {
        shared::Organization {
            id: self.id,
            name: self.name.clone(),
            created_at: self.created_at,
            my_role,
        }
    }
}

#[derive(Debug, Insertable)]
#[diesel(table_name = organizations)]
pub struct NewOrganization<'a> {
    pub name: &'a str,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = org_members)]
pub struct OrgMember {
    pub role: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = org_members)]
pub struct NewOrgMember<'a> {
    pub org_id: Uuid,
    pub user_id: Uuid,
    pub role: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = projects)]
pub struct Project {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub description: String,
    pub default_role: String,
    pub created_at: DateTime<Utc>,
}

impl Project {
    pub fn default_role(&self) -> Result<DefaultProjectRole, ApiError> {
        parse_role(&self.default_role, DefaultProjectRole::parse)
    }

    pub fn to_api(&self, my_role: ProjectRole) -> Result<shared::Project, ApiError> {
        Ok(shared::Project {
            id: self.id,
            org_id: self.org_id,
            name: self.name.clone(),
            description: self.description.clone(),
            default_role: self.default_role()?,
            created_at: self.created_at,
            my_role,
        })
    }
}

#[derive(Debug, Insertable)]
#[diesel(table_name = projects)]
pub struct NewProject<'a> {
    pub org_id: Uuid,
    pub name: &'a str,
    pub description: &'a str,
    pub default_role: &'a str,
    pub created_by: Option<Uuid>,
}

/// Partial project update; `None` fields are left unchanged.
#[derive(Debug, AsChangeset)]
#[diesel(table_name = projects)]
pub struct ProjectChanges<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub default_role: Option<&'a str>,
}

impl ProjectChanges<'_> {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.description.is_none() && self.default_role.is_none()
    }
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = project_members)]
pub struct ProjectMember {
    pub role: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = project_members)]
pub struct NewProjectMember<'a> {
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub role: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = files)]
pub struct File {
    pub id: Uuid,
    pub project_id: Uuid,
    pub path: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = files)]
pub struct NewFile<'a> {
    pub project_id: Uuid,
    pub path: &'a str,
    pub created_by: Option<Uuid>,
}

/// A file version without its content, for listings and history.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = file_versions)]
pub struct FileVersionMeta {
    pub id: Uuid,
    pub file_id: Uuid,
    pub version: i32,
    pub sha256: String,
    pub size_bytes: i64,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

impl FileVersionMeta {
    pub fn to_api(&self, author: Option<User>) -> shared::FileVersionInfo {
        shared::FileVersionInfo {
            id: self.id,
            file_id: self.file_id,
            version: self.version,
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            author: author.map(Into::into),
            message: self.message.clone(),
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Insertable)]
#[diesel(table_name = file_versions)]
pub struct NewFileVersion<'a> {
    pub file_id: Uuid,
    pub version: i32,
    pub content: &'a [u8],
    pub sha256: &'a str,
    pub size_bytes: i64,
    pub author_id: Option<Uuid>,
    pub message: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = audit_log)]
pub struct AuditRow {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub action: String,
    pub detail: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = audit_log)]
pub struct NewAuditRow<'a> {
    pub actor_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub action: &'a str,
    pub detail: serde_json::Value,
}

/// Parse a role column. The database CHECK constraints make failure a sign of
/// corruption, so it surfaces as an internal error.
pub fn parse_role<T>(raw: &str, parse: fn(&str) -> Option<T>) -> Result<T, ApiError> {
    parse(raw).ok_or_else(|| ApiError::Internal(format!("invalid role in database: {raw}")))
}
