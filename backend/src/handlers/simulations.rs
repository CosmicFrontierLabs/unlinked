//! Version-pinned simulations with the same project authorization as file reads.
use crate::{
    access::resolve_project_access,
    error::{ApiError, ApiResult},
    models::{File, User},
    schema::{file_versions, files},
    session::CurrentUser,
    sim_worker, AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use diesel::{
    prelude::*,
    sql_types::{Integer, Nullable, Text, Timestamptz, Uuid as SqlUuid},
};
use shared::{SimulationRequest, SimulationResult, SimulationRun, SimulationStatus};
use std::sync::Arc;
use uuid::Uuid;

#[derive(QueryableByName)]
struct RunRow {
    #[diesel(sql_type=SqlUuid)]
    id: Uuid,
    #[diesel(sql_type=SqlUuid)]
    file_id: Uuid,
    #[diesel(sql_type=SqlUuid)]
    file_version_id: Uuid,
    #[diesel(sql_type=Integer)]
    file_version: i32,
    #[diesel(sql_type=Nullable<SqlUuid>)]
    requested_by: Option<Uuid>,
    #[diesel(sql_type=Text)]
    status: String,
    #[diesel(sql_type=Text)]
    request_json: String,
    #[diesel(sql_type=Nullable<Text>)]
    trace_json: Option<String>,
    #[diesel(sql_type=Nullable<Text>)]
    error: Option<String>,
    #[diesel(sql_type=Timestamptz)]
    created_at: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type=Nullable<Timestamptz>)]
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl RunRow {
    fn result(self) -> ApiResult<SimulationResult> {
        let status = match self.status.as_str() {
            "running" => SimulationStatus::Running,
            "completed" => SimulationStatus::Completed,
            "failed" => SimulationStatus::Failed,
            "cancelled" => SimulationStatus::Cancelled,
            _ => {
                return Err(ApiError::Internal(
                    "invalid stored simulation status".into(),
                ))
            }
        };
        let request = serde_json::from_str(&self.request_json)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let trace = self
            .trace_json
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(SimulationResult {
            run: SimulationRun {
                id: self.id,
                file_id: self.file_id,
                file_version_id: self.file_version_id,
                file_version: self.file_version,
                requested_by: self.requested_by,
                status,
                request,
                error: self.error,
                created_at: self.created_at,
                finished_at: self.finished_at,
            },
            trace,
        })
    }
}
const SELECT_RUN:&str="SELECT r.id, v.file_id, r.file_version_id, v.version AS file_version, r.requested_by, r.status, r.request::text AS request_json, r.error, r.created_at, r.finished_at";
fn load(conn: &mut PgConnection, id: Uuid, trace: bool) -> ApiResult<SimulationResult> {
    // Workers have a 30-second compute deadline plus bounded stream/DB waits.
    // Reconcile abandoned records after a crash without cancelling other live
    // instances' recently started runs.
    diesel::sql_query("UPDATE simulation_runs SET status='failed',error='simulation worker expired or server restarted',finished_at=NOW() WHERE id=$1 AND status='running' AND created_at < NOW()-INTERVAL '5 minutes'")
        .bind::<SqlUuid,_>(id).execute(conn)?;

    let trace_column = if trace { "r.trace::text" } else { "NULL::text" };
    let query=format!("{SELECT_RUN}, {trace_column} AS trace_json FROM simulation_runs r JOIN file_versions v ON v.id=r.file_version_id WHERE r.id=$1");
    diesel::sql_query(query)
        .bind::<SqlUuid, _>(id)
        .get_result::<RunRow>(conn)?
        .result()
}
fn authorize_file(conn: &mut PgConnection, user: User, id: Uuid) -> ApiResult<File> {
    let file = files::table
        .find(id)
        .filter(files::deleted_at.is_null())
        .select(File::as_select())
        .first::<File>(conn)?;
    resolve_project_access(conn, user, file.project_id)?;
    Ok(file)
}
#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type=SqlUuid)]
    id: Uuid,
}
fn authorize_run(conn: &mut PgConnection, user: User, id: Uuid) -> ApiResult<()> {
    let file=diesel::sql_query("SELECT v.file_id AS id FROM simulation_runs r JOIN file_versions v ON v.id=r.file_version_id WHERE r.id=$1").bind::<SqlUuid,_>(id).get_result::<IdRow>(conn)?;
    authorize_file(conn, user, file.id)?;
    Ok(())
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/files/:file_id/simulations", post(run).get(list))
        .route("/api/simulations/:run_id", get(detail))
}

fn validate(request: &SimulationRequest) -> ApiResult<()> {
    let o = &request.options;
    if !o.start.is_finite()
        || !o.stop.is_finite()
        || !o.step.is_finite()
        || o.step <= 0.0
        || o.stop < o.start
        || o.max_samples == 0
    {
        return Err(ApiError::BadRequest(
            "require finite start <= stop, positive step and sample budget".into(),
        ));
    }
    if request.version.is_some_and(|v| v < 1) {
        return Err(ApiError::BadRequest("version must be positive".into()));
    }
    if request.workspace.len() > 256
        || request.workspace.iter().any(|(k, v)| {
            k.len() > 63
                || k.is_empty()
                || !k.as_bytes()[0].is_ascii_alphabetic()
                || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || v.len() > 4096
        })
    {
        return Err(ApiError::BadRequest(
            "workspace requires <=256 named scalar expressions, each <=4096 bytes".into(),
        ));
    }
    Ok(())
}

pub struct ActiveRun {
    pub run: SimulationRun,
    pub worker: sim_worker::Worker,
}
/// Called by both HTTP and authenticated WebSocket handlers. Authorization and
/// immutable version selection happen together before any worker receives bytes.
pub async fn begin(
    state: Arc<AppState>,
    user: User,
    file_id: Uuid,
    mut request: SimulationRequest,
) -> ApiResult<ActiveRun> {
    validate(&request)?;
    request.options.max_samples = request.options.max_samples.min(25_001);
    let opts = request.options.clone();
    let workspace = request.workspace.clone();
    let actor = user.id;
    let (record,path,bytes)=state.db(move |conn| {
  conn.transaction::<_,ApiError,_>(|conn| {
   let file=authorize_file(conn,user,file_id)?;
   let mut query=file_versions::table.filter(file_versions::file_id.eq(file_id)).into_boxed();
   if let Some(version)=request.version {query=query.filter(file_versions::version.eq(version));}
   let (version_id,version,bytes)=query.order(file_versions::version.desc()).select((file_versions::id,file_versions::version,file_versions::content)).first::<(Uuid,i32,Vec<u8>)>(conn)?;
   request.version=Some(version);
   let json=serde_json::to_string(&request).map_err(|e|ApiError::BadRequest(e.to_string()))?;
   let id=Uuid::new_v4();
   diesel::sql_query("INSERT INTO simulation_runs (id,file_version_id,requested_by,status,request) VALUES ($1,$2,$3,'running',$4::jsonb)").bind::<SqlUuid,_>(id).bind::<SqlUuid,_>(version_id).bind::<SqlUuid,_>(actor).bind::<Text,_>(json).execute(conn)?;
   crate::audit::record(conn,actor,None,Some(file.project_id),"simulation.started",serde_json::json!({"run_id":id,"file_version_id":version_id}))?;
   Ok((load(conn,id,false)?.run,file.path,bytes))
  })
 }).await?;
    let run_id = record.id;
    let pool = state.db_pool.clone();
    let worker = sim_worker::start(actor, path, bytes, opts, workspace, move |result| {
        let mut conn = pool.get().map_err(|e| e.to_string())?;
        finish(&mut conn, run_id, result).map_err(|e| e.to_string())
    });
    match worker {
        Ok(worker) => Ok(ActiveRun {
            run: record,
            worker,
        }),
        Err(message) => {
            let error = message.clone();
            state
                .db(move |conn| finish(conn, run_id, &Err(error)))
                .await?;
            Err(ApiError::Conflict(message))
        }
    }
}
fn finish(
    conn: &mut PgConnection,
    id: Uuid,
    result: &Result<unlinked_sim::Trace, String>,
) -> ApiResult<()> {
    let (status, trace, error) = match result {
        Ok(trace) => (
            "completed",
            Some(serde_json::to_string(trace).map_err(|e| ApiError::Internal(e.to_string()))?),
            None,
        ),
        Err(error) => (
            if error.contains("cancelled") || error.contains("disconnected") {
                "cancelled"
            } else {
                "failed"
            },
            None,
            Some(error.clone()),
        ),
    };
    diesel::sql_query("UPDATE simulation_runs SET status=$2, trace=$3::jsonb,error=$4,finished_at=NOW() WHERE id=$1 AND status='running'").bind::<SqlUuid,_>(id).bind::<Text,_>(status).bind::<Nullable<Text>,_>(trace).bind::<Nullable<Text>,_>(error).execute(conn)?;
    Ok(())
}

pub async fn result(
    state: Arc<AppState>,
    user: User,
    id: Uuid,
    trace: bool,
) -> ApiResult<SimulationResult> {
    state
        .db(move |conn| {
            authorize_run(conn, user, id)?;
            load(conn, id, trace)
        })
        .await
}
async fn run(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
    Path(file_id): Path<Uuid>,
    Json(request): Json<SimulationRequest>,
) -> ApiResult<Json<SimulationResult>> {
    let mut active = begin(state.clone(), user.clone(), file_id, request).await?;
    while let Some(event) = active.worker.events.recv().await {
        match event {
            sim_worker::Event::Completed => break,
            sim_worker::Event::Failed(message) => {
                tracing::debug!(%message,"simulation worker ended");
                break;
            }
            _ => {}
        }
    }
    Ok(Json(result(state, user, active.run.id, true).await?))
}
async fn detail(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SimulationResult>> {
    Ok(Json(result(state, user, id, true).await?))
}
async fn list(
    State(state): State<Arc<AppState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SimulationRun>>> {
    let runs=state.db(move |conn| {
  authorize_file(conn,user,id)?;
  let ids=diesel::sql_query("SELECT r.id FROM simulation_runs r JOIN file_versions v ON v.id=r.file_version_id WHERE v.file_id=$1 ORDER BY r.created_at DESC LIMIT 100").bind::<SqlUuid,_>(id).load::<IdRow>(conn)?;
  ids.into_iter().map(|row|load(conn,row.id,false).map(|r|r.run)).collect::<ApiResult<Vec<_>>>()
 }).await?;
    Ok(Json(runs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{db_app, sign_up, Client};
    use axum::http::{Method, StatusCode};
    use serde_json::json;
    #[tokio::test]
    async fn version_pinning_viewer_execution_and_tenant_isolation() {
        let Some((state, app)) = db_app() else {
            return;
        };
        let owner = sign_up(&state, "sim-owner");
        let viewer = sign_up(&state, "sim-viewer");
        let outsider = sign_up(&state, "sim-outsider");
        let own = Client::new(&app, &owner);
        let view = Client::new(&app, &viewer);
        let outside = Client::new(&app, &outsider);
        let org: shared::Organization = own
            .json(
                Method::POST,
                "/api/orgs",
                &json!({"name":"simulation test"}),
            )
            .await
            .json();
        let project: shared::Project = own
            .json(
                Method::POST,
                &format!("/api/orgs/{}/projects", org.id),
                &json!({"name":"control","default_role":"none"}),
            )
            .await
            .json();
        let grant = own
            .json(
                Method::POST,
                &format!("/api/projects/{}/members", project.id),
                &json!({"email":viewer.email(),"role":"viewer"}),
            )
            .await;
        assert!(grant.status.is_success());
        let source =
            include_bytes!("../../../crates/unlinked-cli/tests/fixtures/scalar.mdl").to_vec();
        let uploaded: shared::FileInfo = own
            .upload(project.id, "scalar.mdl", source.clone())
            .await
            .json();
        let route = format!("/api/files/{}/simulations", uploaded.id);
        let request = SimulationRequest {
            options: unlinked_sim::Options {
                stop: 0.2,
                step: 0.1,
                ..Default::default()
            },
            workspace: Default::default(),
            version: None,
        };
        let denied = outside.json(Method::POST, &route, &request).await;
        assert_eq!(denied.status, StatusCode::NOT_FOUND);
        let response = view.json(Method::POST, &route, &request).await;
        assert_eq!(response.status, StatusCode::OK);
        let finished: SimulationResult = response.json();
        assert_eq!(finished.run.status, SimulationStatus::Completed);
        assert_eq!(finished.run.file_version, 1);
        assert_eq!(finished.run.file_version_id, uploaded.latest.id);
        assert_eq!(finished.trace.as_ref().unwrap().signals["2"], vec![6.0; 3]);
        let result_route = format!("/api/simulations/{}", finished.run.id);
        assert_eq!(
            outside.get(&result_route).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(outside.get(&route).await.status, StatusCode::NOT_FOUND);
        let changed = String::from_utf8(source)
            .unwrap()
            .replace("Gain \"3\"", "Gain \"4\"")
            .into_bytes();
        let updated: shared::FileInfo = own.upload(project.id, "scalar.mdl", changed).await.json();
        assert_eq!(updated.latest.version, 2);
        let stored: SimulationResult = view.get(&result_route).await.json();
        assert_eq!(stored.run.file_version, 1);
        assert_eq!(stored.trace.unwrap().signals["2"], vec![6.0; 3]);
        let latest: SimulationResult = view.json(Method::POST, &route, &request).await.json();
        assert_eq!(latest.run.file_version, 2);
        assert_eq!(latest.trace.unwrap().signals["2"], vec![8.0; 3]);
        let broken =
            include_bytes!("../../../crates/unlinked-cli/tests/fixtures/scalar.mdl").to_vec();
        let broken = String::from_utf8(broken)
            .unwrap()
            .replace("BlockType Gain", "BlockType Unsupported")
            .into_bytes();
        own.upload(project.id, "scalar.mdl", broken).await;
        let failed: SimulationResult = view.json(Method::POST, &route, &request).await.json();
        assert_eq!(failed.run.status, SimulationStatus::Failed);
        assert!(failed.trace.is_none());
        assert!(failed.run.error.unwrap().contains("unsupported block type"));
        let removed = own
            .delete(&format!(
                "/api/projects/{}/members/{}",
                project.id, viewer.user.id
            ))
            .await;
        assert!(removed.status.is_success());
        assert_eq!(view.get(&result_route).await.status, StatusCode::NOT_FOUND);
        let invalid = SimulationRequest {
            version: Some(999),
            ..request
        };
        assert_eq!(
            own.json(Method::POST, &route, &invalid).await.status,
            StatusCode::NOT_FOUND
        );
    }
    #[test]
    fn request_limits_reject_invalid_inputs() {
        let mut request = SimulationRequest {
            options: Default::default(),
            workspace: Default::default(),
            version: None,
        };
        request.options.step = 0.0;
        assert!(validate(&request).is_err());
        request.options.step = 0.1;
        request.workspace.insert("bad name".into(), "1".into());
        assert!(validate(&request).is_err());
    }
}
