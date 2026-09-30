use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

// ---------------------------------------------------------------------------
// WebSocket endpoint definition — single source of truth for server + client
// ---------------------------------------------------------------------------

/// The main application WebSocket endpoint.
pub struct AppSocket;

impl WsEndpoint for AppSocket {
    const PATH: &'static str = "/ws";
    type ServerMsg = ServerMsg;
    type ClientMsg = ClientMsg;
}

/// Messages sent from the server to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    /// Heartbeat to keep connection alive
    Heartbeat,

    /// Error from server
    Error { message: String },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },
}

/// Messages sent from the client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// Ping — server should respond with Heartbeat
    Ping,
}

// ---------------------------------------------------------------------------
// HTTP API types
// ---------------------------------------------------------------------------

/// Health check response from `/api/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Body of every non-2xx JSON response from `/api/*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// A user's role within an organization, ordered from least to most
/// privileged so `role >= OrgRole::Admin` reads naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    Member,
    Admin,
    Owner,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Member => "member",
            OrgRole::Admin => "admin",
            OrgRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member" => Some(OrgRole::Member),
            "admin" => Some(OrgRole::Admin),
            "owner" => Some(OrgRole::Owner),
            _ => None,
        }
    }
}

/// A user's effective role on a project, ordered from least to most
/// privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectRole {
    Viewer,
    Editor,
    Owner,
}

impl ProjectRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectRole::Viewer => "viewer",
            ProjectRole::Editor => "editor",
            ProjectRole::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "viewer" => Some(ProjectRole::Viewer),
            "editor" => Some(ProjectRole::Editor),
            "owner" => Some(ProjectRole::Owner),
            _ => None,
        }
    }
}

/// The role every member of a project's organization receives on that project
/// unless they hold an explicit project membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultProjectRole {
    None,
    Viewer,
    Editor,
}

impl DefaultProjectRole {
    pub fn as_str(self) -> &'static str {
        match self {
            DefaultProjectRole::None => "none",
            DefaultProjectRole::Viewer => "viewer",
            DefaultProjectRole::Editor => "editor",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(DefaultProjectRole::None),
            "viewer" => Some(DefaultProjectRole::Viewer),
            "editor" => Some(DefaultProjectRole::Editor),
            _ => None,
        }
    }

    /// The project role this default grants, if any.
    pub fn project_role(self) -> Option<ProjectRole> {
        match self {
            DefaultProjectRole::None => None,
            DefaultProjectRole::Viewer => Some(ProjectRole::Viewer),
            DefaultProjectRole::Editor => Some(ProjectRole::Editor),
        }
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// Response from `GET /api/auth/providers`: which login buttons to show.
///
/// `providers` holds the keys accepted by `/api/auth/login/:provider`
/// (`google`, `github`). In dev mode the login page should instead link to
/// `/api/auth/dev-login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthProvidersResponse {
    pub providers: Vec<String>,
    pub dev_mode: bool,
}

/// Public profile of a user, returned by `GET /api/me` and in member lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Organizations
// ---------------------------------------------------------------------------

/// An organization as seen by one of its members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// The caller's role in this organization.
    pub my_role: OrgRole,
}

/// Body of `POST /api/orgs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrgRequest {
    pub name: String,
}

/// Body of `PATCH /api/orgs/:org_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateOrgRequest {
    pub name: String,
}

/// One row of `GET /api/orgs/:org_id/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMember {
    pub user: UserInfo,
    pub role: OrgRole,
    pub joined_at: DateTime<Utc>,
}

/// Body of `POST /api/orgs/:org_id/members`. The user must already have
/// signed in once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddOrgMemberRequest {
    pub email: String,
    pub role: OrgRole,
}

/// Body of `PATCH /api/orgs/:org_id/members/:user_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateOrgMemberRequest {
    pub role: OrgRole,
}

/// One row of `GET /api/orgs/:org_id/audit`, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub actor: Option<UserInfo>,
    pub project_id: Option<Uuid>,
    pub action: String,
    pub detail: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// A project as seen by a user with a role on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub description: String,
    pub default_role: DefaultProjectRole,
    pub created_at: DateTime<Utc>,
    /// The caller's effective role on this project.
    pub my_role: ProjectRole,
}

/// Body of `POST /api/orgs/:org_id/projects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Defaults to `viewer` when omitted.
    #[serde(default)]
    pub default_role: Option<DefaultProjectRole>,
}

/// Body of `PATCH /api/projects/:project_id`. Absent fields are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default_role: Option<DefaultProjectRole>,
}

/// One row of `GET /api/projects/:project_id/members` (explicit members only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMember {
    pub user: UserInfo,
    pub role: ProjectRole,
    pub added_at: DateTime<Utc>,
}

/// Body of `POST /api/projects/:project_id/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddProjectMemberRequest {
    pub email: String,
    pub role: ProjectRole,
}

/// Body of `PATCH /api/projects/:project_id/members/:user_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProjectMemberRequest {
    pub role: ProjectRole,
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Metadata for one immutable file version (content is fetched separately).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersionInfo {
    pub id: Uuid,
    pub file_id: Uuid,
    /// 1-based, increasing by one per upload.
    pub version: i32,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
    pub size_bytes: i64,
    pub author: Option<UserInfo>,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

/// A live file in a project together with its latest version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Relative `/`-separated path within the project.
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub latest: FileVersionInfo,
}

/// Body of `PATCH /api/projects/:project_id/files/:file_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameFileRequest {
    pub path: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::fmt::Debug;

    fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let json = serde_json::to_string(value).unwrap();
        let parsed: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&parsed, value);
    }

    fn user() -> UserInfo {
        UserInfo {
            id: Uuid::new_v4(),
            email: "ada@example.com".to_string(),
            name: Some("Ada".to_string()),
            avatar_url: None,
        }
    }

    fn version() -> FileVersionInfo {
        FileVersionInfo {
            id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            version: 3,
            sha256: "ab".repeat(32),
            size_bytes: 1234,
            author: Some(user()),
            message: "tune gains".to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn server_msg_heartbeat_roundtrip() {
        let msg = ServerMsg::Heartbeat;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ServerMsg::Heartbeat));
    }

    #[test]
    fn server_msg_error_roundtrip() {
        let msg = ServerMsg::Error {
            message: "something broke".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMsg::Error { message } => assert_eq!(message, "something broke"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_msg_shutdown_roundtrip() {
        let msg = ServerMsg::ServerShutdown {
            reason: "restarting".to_string(),
            reconnect_delay_ms: 1000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMsg::ServerShutdown {
                reason,
                reconnect_delay_ms,
            } => {
                assert_eq!(reason, "restarting");
                assert_eq!(reconnect_delay_ms, 1000);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn client_msg_ping_roundtrip() {
        let msg = ClientMsg::Ping;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMsg::Ping));
    }

    #[test]
    fn health_response_roundtrip() {
        let json = serde_json::to_string(&HealthResponse {
            status: "ok".to_string(),
        })
        .unwrap();
        let parsed: HealthResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, "ok");
    }

    #[test]
    fn error_response_roundtrip() {
        roundtrip(&ErrorResponse {
            error: "not found".to_string(),
        });
    }

    #[test]
    fn org_role_roundtrip_and_order() {
        for role in [OrgRole::Member, OrgRole::Admin, OrgRole::Owner] {
            roundtrip(&role);
            assert_eq!(OrgRole::parse(role.as_str()), Some(role));
            assert_eq!(
                serde_json::to_string(&role).unwrap(),
                format!("\"{}\"", role.as_str())
            );
        }
        assert!(OrgRole::Owner > OrgRole::Admin && OrgRole::Admin > OrgRole::Member);
        assert_eq!(OrgRole::parse("root"), None);
    }

    #[test]
    fn project_role_roundtrip_and_order() {
        for role in [ProjectRole::Viewer, ProjectRole::Editor, ProjectRole::Owner] {
            roundtrip(&role);
            assert_eq!(ProjectRole::parse(role.as_str()), Some(role));
            assert_eq!(
                serde_json::to_string(&role).unwrap(),
                format!("\"{}\"", role.as_str())
            );
        }
        assert!(ProjectRole::Owner > ProjectRole::Editor);
        assert!(ProjectRole::Editor > ProjectRole::Viewer);
        assert_eq!(ProjectRole::parse("admin"), None);
    }

    #[test]
    fn default_project_role_roundtrip() {
        for role in [
            DefaultProjectRole::None,
            DefaultProjectRole::Viewer,
            DefaultProjectRole::Editor,
        ] {
            roundtrip(&role);
            assert_eq!(DefaultProjectRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(DefaultProjectRole::None.project_role(), None);
        assert_eq!(
            DefaultProjectRole::Editor.project_role(),
            Some(ProjectRole::Editor)
        );
        assert_eq!(DefaultProjectRole::parse("owner"), None);
    }

    #[test]
    fn auth_providers_response_roundtrip() {
        roundtrip(&AuthProvidersResponse {
            providers: vec!["google".to_string(), "github".to_string()],
            dev_mode: false,
        });
    }

    #[test]
    fn user_info_roundtrip() {
        roundtrip(&user());
    }

    #[test]
    fn organization_roundtrip() {
        roundtrip(&Organization {
            id: Uuid::new_v4(),
            name: "Acme".to_string(),
            created_at: Utc::now(),
            my_role: OrgRole::Admin,
        });
    }

    #[test]
    fn create_org_request_roundtrip() {
        roundtrip(&CreateOrgRequest {
            name: "Acme".to_string(),
        });
    }

    #[test]
    fn update_org_request_roundtrip() {
        roundtrip(&UpdateOrgRequest {
            name: "Acme Corp".to_string(),
        });
    }

    #[test]
    fn org_member_roundtrip() {
        roundtrip(&OrgMember {
            user: user(),
            role: OrgRole::Owner,
            joined_at: Utc::now(),
        });
    }

    #[test]
    fn add_org_member_request_roundtrip() {
        roundtrip(&AddOrgMemberRequest {
            email: "bob@example.com".to_string(),
            role: OrgRole::Member,
        });
    }

    #[test]
    fn update_org_member_request_roundtrip() {
        roundtrip(&UpdateOrgMemberRequest {
            role: OrgRole::Admin,
        });
    }

    #[test]
    fn audit_entry_roundtrip() {
        roundtrip(&AuditEntry {
            id: Uuid::new_v4(),
            actor: Some(user()),
            project_id: Some(Uuid::new_v4()),
            action: "file.upload".to_string(),
            detail: serde_json::json!({ "path": "models/plant.slx", "version": 2 }),
            created_at: Utc::now(),
        });
    }

    #[test]
    fn project_roundtrip() {
        roundtrip(&Project {
            id: Uuid::new_v4(),
            org_id: Uuid::new_v4(),
            name: "Autopilot".to_string(),
            description: "Pitch loop".to_string(),
            default_role: DefaultProjectRole::Viewer,
            created_at: Utc::now(),
            my_role: ProjectRole::Editor,
        });
    }

    #[test]
    fn create_project_request_roundtrip() {
        roundtrip(&CreateProjectRequest {
            name: "Autopilot".to_string(),
            description: String::new(),
            default_role: Some(DefaultProjectRole::None),
        });
        let minimal: CreateProjectRequest = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert_eq!(minimal.default_role, None);
        assert_eq!(minimal.description, "");
    }

    #[test]
    fn update_project_request_roundtrip() {
        roundtrip(&UpdateProjectRequest {
            name: Some("Renamed".to_string()),
            description: None,
            default_role: Some(DefaultProjectRole::Editor),
        });
        let empty: UpdateProjectRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, UpdateProjectRequest::default());
    }

    #[test]
    fn project_member_roundtrip() {
        roundtrip(&ProjectMember {
            user: user(),
            role: ProjectRole::Viewer,
            added_at: Utc::now(),
        });
    }

    #[test]
    fn add_project_member_request_roundtrip() {
        roundtrip(&AddProjectMemberRequest {
            email: "carol@example.com".to_string(),
            role: ProjectRole::Editor,
        });
    }

    #[test]
    fn update_project_member_request_roundtrip() {
        roundtrip(&UpdateProjectMemberRequest {
            role: ProjectRole::Owner,
        });
    }

    #[test]
    fn file_version_info_roundtrip() {
        roundtrip(&version());
    }

    #[test]
    fn file_info_roundtrip() {
        roundtrip(&FileInfo {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            path: "models/plant.slx".to_string(),
            created_at: Utc::now(),
            latest: version(),
        });
    }

    #[test]
    fn rename_file_request_roundtrip() {
        roundtrip(&RenameFileRequest {
            path: "models/plant_v2.slx".to_string(),
        });
    }
}
