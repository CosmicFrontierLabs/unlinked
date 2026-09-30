//! Project files and their immutable version history.
//!
//! Uploads are raw request bodies (`POST /api/projects/:project_id/files
//! ?path=<relative path>&message=<text>`); each creates a new version of the
//! live file at that path, creating the file on first upload. Body size is
//! capped by `MAX_UPLOAD_BYTES` at the router.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use diesel::dsl::max;
use diesel::prelude::*;
use diesel::PgConnection;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use shared::{FileInfo, FileVersionInfo, ProjectRole, RenameFileRequest};
use uuid::Uuid;

use crate::access::ProjectAccess;
use crate::audit;
use crate::error::{ApiError, ApiResult};
use crate::models::{File, FileVersionMeta, NewFile, NewFileVersion, User};
use crate::schema::{file_versions, files, users};
use crate::AppState;

const MAX_PATH_BYTES: usize = 1024;
const MAX_MESSAGE_CHARS: usize = 10_000;

/// Validate a project-relative file path.
///
/// Paths are `/`-separated and relative. Anything that could escape the
/// project or be interpreted differently by another consumer (absolute paths,
/// drive letters, `.`/`..` segments, backslashes, empty segments, NUL and other
/// control characters) is rejected rather than normalized.
pub fn validate_path(path: &str) -> ApiResult<&str> {
    let reject = |why: &str| Err(ApiError::BadRequest(format!("invalid path: {why}")));
    if path.is_empty() {
        return reject("empty");
    }
    if path.len() > MAX_PATH_BYTES {
        return reject("too long");
    }
    if path.starts_with('/') {
        return reject("must be relative");
    }
    if path.contains('\\') {
        return reject("backslashes are not allowed");
    }
    if path.chars().any(char::is_control) {
        return reject("control characters are not allowed");
    }
    for (i, segment) in path.split('/').enumerate() {
        match segment {
            "" => return reject("empty segment"),
            "." | ".." => return reject("'.' and '..' segments are not allowed"),
            s if i == 0 && s.ends_with(':') => return reject("must be relative"),
            _ => {}
        }
    }
    Ok(path)
}

fn validate_message(message: &str) -> ApiResult<()> {
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(ApiError::BadRequest(format!(
            "message must be at most {MAX_MESSAGE_CHARS} characters"
        )));
    }
    Ok(())
}

/// A live (not deleted) file in the given project.
fn live_file(conn: &mut PgConnection, project_id: Uuid, file_id: Uuid) -> ApiResult<File> {
    Ok(files::table
        .filter(files::id.eq(file_id))
        .filter(files::project_id.eq(project_id))
        .filter(files::deleted_at.is_null())
        .select(File::as_select())
        .first(conn)?)
}

/// The newest version of each file, keyed by file id.
fn latest_versions(
    conn: &mut PgConnection,
    file_ids: &[Uuid],
) -> ApiResult<HashMap<Uuid, FileVersionInfo>> {
    Ok(file_versions::table
        .left_join(users::table)
        .filter(file_versions::file_id.eq_any(file_ids))
        .distinct_on(file_versions::file_id)
        .order((file_versions::file_id, file_versions::version.desc()))
        .select((FileVersionMeta::as_select(), Option::<User>::as_select()))
        .load::<(FileVersionMeta, Option<User>)>(conn)?
        .into_iter()
        .map(|(meta, author)| (meta.file_id, meta.to_api(author)))
        .collect())
}

fn file_info(file: File, latest: Option<FileVersionInfo>) -> ApiResult<FileInfo> {
    let latest =
        latest.ok_or_else(|| ApiError::Internal(format!("file {} has no versions", file.id)))?;
    Ok(FileInfo {
        id: file.id,
        project_id: file.project_id,
        path: file.path,
        created_at: file.created_at,
        latest,
    })
}

fn load_file_info(conn: &mut PgConnection, file: File) -> ApiResult<FileInfo> {
    let latest = latest_versions(conn, &[file.id])?.remove(&file.id);
    file_info(file, latest)
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
) -> ApiResult<Json<Vec<FileInfo>>> {
    state
        .db(move |conn| {
            let live: Vec<File> = files::table
                .filter(files::project_id.eq(access.project.id))
                .filter(files::deleted_at.is_null())
                .order(files::path)
                .select(File::as_select())
                .load(conn)?;
            let ids: Vec<Uuid> = live.iter().map(|f| f.id).collect();
            let mut latest = latest_versions(conn, &ids)?;
            live.into_iter()
                .map(|f| {
                    let v = latest.remove(&f.id);
                    file_info(f, v)
                })
                .collect::<ApiResult<Vec<_>>>()
                .map(Json)
        })
        .await
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    path: String,
    #[serde(default)]
    message: String,
}

/// Store the request body as a new version of the file at `path`.
pub async fn upload(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<FileInfo>)> {
    access.require(ProjectRole::Editor)?;
    let path = validate_path(&query.path)?.to_string();
    validate_message(&query.message)?;
    let sha256 = hex::encode(Sha256::digest(&body));
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let project_id = access.project.id;
                let existing = files::table
                    .filter(files::project_id.eq(project_id))
                    .filter(files::path.eq(&path))
                    .filter(files::deleted_at.is_null())
                    .select(File::as_select())
                    .for_update()
                    .first(conn)
                    .optional()?;
                let file = match existing {
                    Some(file) => file,
                    None => diesel::insert_into(files::table)
                        .values(NewFile {
                            project_id,
                            path: &path,
                            created_by: Some(access.user.id),
                        })
                        .returning(File::as_returning())
                        .get_result(conn)?,
                };
                let next = file_versions::table
                    .filter(file_versions::file_id.eq(file.id))
                    .select(max(file_versions::version))
                    .first::<Option<i32>>(conn)?
                    .unwrap_or(0)
                    + 1;
                let size_bytes = i64::try_from(body.len())
                    .map_err(|_| ApiError::BadRequest("file too large".to_string()))?;
                let meta: FileVersionMeta = diesel::insert_into(file_versions::table)
                    .values(NewFileVersion {
                        file_id: file.id,
                        version: next,
                        content: &body,
                        sha256: &sha256,
                        size_bytes,
                        author_id: Some(access.user.id),
                        message: &query.message,
                    })
                    .returning(FileVersionMeta::as_returning())
                    .get_result(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(project_id),
                    "file.upload",
                    serde_json::json!({
                        "file_id": file.id,
                        "path": path,
                        "version": next,
                        "sha256": sha256,
                        "size_bytes": size_bytes,
                    }),
                )?;
                let latest = meta.to_api(Some(access.user.clone()));
                Ok((StatusCode::CREATED, Json(file_info(file, Some(latest))?)))
            })
        })
        .await
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<FileInfo>> {
    state
        .db(move |conn| {
            let file = live_file(conn, access.project.id, file_id)?;
            load_file_info(conn, file).map(Json)
        })
        .await
}

pub async fn rename(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RenameFileRequest>,
) -> ApiResult<Json<FileInfo>> {
    access.require(ProjectRole::Editor)?;
    let path = validate_path(&req.path)?.to_string();
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let old = live_file(conn, access.project.id, file_id)?;
                let file: File = diesel::update(files::table.find(old.id))
                    .set(files::path.eq(&path))
                    .returning(File::as_returning())
                    .get_result(conn)
                    .map_err(|e| match ApiError::from(e) {
                        ApiError::Conflict(_) => {
                            ApiError::Conflict("a file with that path already exists".to_string())
                        }
                        other => other,
                    })?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(access.project.id),
                    "file.rename",
                    serde_json::json!({ "file_id": file.id, "from": old.path, "to": path }),
                )?;
                load_file_info(conn, file).map(Json)
            })
        })
        .await
}

/// Soft-delete a file. Its history is retained but no longer served, and the
/// path becomes free for a new file.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    access.require(ProjectRole::Editor)?;
    state
        .db(move |conn| {
            conn.transaction(|conn| {
                let file = live_file(conn, access.project.id, file_id)?;
                diesel::update(files::table.find(file.id))
                    .set(files::deleted_at.eq(Utc::now()))
                    .execute(conn)?;
                audit::record(
                    conn,
                    access.user.id,
                    Some(access.project.org_id),
                    Some(access.project.id),
                    "file.delete",
                    serde_json::json!({ "file_id": file.id, "path": file.path }),
                )?;
                Ok(StatusCode::NO_CONTENT)
            })
        })
        .await
}

/// Version history, newest first.
pub async fn versions(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Vec<FileVersionInfo>>> {
    state
        .db(move |conn| {
            let file = live_file(conn, access.project.id, file_id)?;
            Ok(Json(
                file_versions::table
                    .left_join(users::table)
                    .filter(file_versions::file_id.eq(file.id))
                    .order(file_versions::version.desc())
                    .select((FileVersionMeta::as_select(), Option::<User>::as_select()))
                    .load::<(FileVersionMeta, Option<User>)>(conn)?
                    .into_iter()
                    .map(|(meta, author)| meta.to_api(author))
                    .collect(),
            ))
        })
        .await
}

/// Raw bytes of a stored version, served as an opaque attachment so the
/// browser never renders user content on this origin.
fn content_response(path: &str, sha256: &str, content: Vec<u8>) -> Response {
    let name: String = path
        .rsplit('/')
        .next()
        .unwrap_or("download")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    let etag = HeaderValue::from_str(&format!("\"{sha256}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("\"\""));
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::ETAG, etag),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            ),
        ],
        content,
    )
        .into_response()
}

/// Download the latest version of a file.
pub async fn latest_content(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    state
        .db(move |conn| {
            let file = live_file(conn, access.project.id, file_id)?;
            let (sha256, content): (String, Vec<u8>) = file_versions::table
                .filter(file_versions::file_id.eq(file.id))
                .order(file_versions::version.desc())
                .select((file_versions::sha256, file_versions::content))
                .first(conn)?;
            Ok(content_response(&file.path, &sha256, content))
        })
        .await
}

/// Download one specific version of a file.
pub async fn version_content(
    State(state): State<Arc<AppState>>,
    access: ProjectAccess,
    Path((_, file_id, version_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Response> {
    state
        .db(move |conn| {
            let file = live_file(conn, access.project.id, file_id)?;
            let (sha256, content): (String, Vec<u8>) = file_versions::table
                .filter(file_versions::id.eq(version_id))
                .filter(file_versions::file_id.eq(file.id))
                .select((file_versions::sha256, file_versions::content))
                .first(conn)?;
            Ok(content_response(&file.path, &sha256, content))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_relative_paths() {
        for ok in [
            "plant.slx",
            "models/plant.slx",
            "a/b/c/d.m",
            "with space/file name.mdl",
            "..hidden/..x",
            "unicodé/ファイル.slx",
        ] {
            assert_eq!(validate_path(ok).unwrap(), ok);
        }
    }

    #[test]
    fn rejects_traversal_and_ambiguous_paths() {
        for bad in [
            "",
            "/etc/passwd",
            "../secret",
            "models/../../secret",
            "models/..",
            "./plant.slx",
            "models/./plant.slx",
            "models\\plant.slx",
            "..\\..\\windows",
            "models//plant.slx",
            "models/",
            "C:/Windows/system32",
            "C:",
            "nul\0byte",
            "line\nbreak",
        ] {
            assert!(validate_path(bad).is_err(), "accepted {bad:?}");
        }
        assert!(validate_path(&"a".repeat(MAX_PATH_BYTES + 1)).is_err());
    }
}
