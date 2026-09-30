use super::simulations::{self, ActiveRun};
use crate::{
    error::{ApiError, ApiResult},
    session::{session_token, session_user, WsUser},
    sim_worker::Event,
    AppState,
};
use axum::{
    extract::{ws::WebSocketUpgrade, State},
    response::Response,
};
use shared::{SimulationClientMsg as Client, SimulationServerMsg as Server, SimulationSocket};
use std::sync::Arc;
use tower_cookies::Cookies;

type Connection = ws_bridge::server::Connection<SimulationSocket>;
async fn send(conn: &mut Connection, message: Server) -> bool {
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(10), conn.send(message)).await,
        Ok(Ok(()))
    )
}
pub async fn handler(
    State(state): State<Arc<AppState>>,
    WsUser(user): WsUser,
    cookies: Cookies,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let token = session_token(&cookies, &state).ok_or(ApiError::Unauthorized)?;
    Ok(ws_bridge::server::upgrade::<SimulationSocket, _, _>(
        ws.max_message_size(128 * 1024).max_frame_size(128 * 1024),
        move |mut conn| async move {
            let mut active: Option<ActiveRun> = None;
            if !send(&mut conn, Server::Heartbeat).await {
                return;
            }
            loop {
                if let Some(running) = active.as_mut() {
                    let run_id = running.run.id;
                    tokio::select! {
                     message=conn.recv()=>{
                      match message {
                       Some(Ok(Client::Ping))=>{if !send(&mut conn,Server::Heartbeat).await{return;}},
                       Some(Ok(Client::CancelSimulation{run_id:id})) if id==run_id=>running.worker.cancel(),
                       Some(Ok(_))=>{if !send(&mut conn,Server::Error{message:"one active simulation per socket; cancel or wait for completion".into()}).await{return;}},
                       Some(Err(_))=>{if !send(&mut conn,Server::Error{message:"invalid simulation message".into()}).await{return;}},
                       None=>return,
                      }
                     },
                     event=running.worker.events.recv()=>{
                      match event {
                       Some(Event::Started{signals})=>{
                        if !send(&mut conn,Server::SimulationStarted{run_id,signals}).await{return;}
                       },
                       Some(Event::Samples{time,values})=>{if !send(&mut conn,Server::SimulationSamples{run_id,time,values}).await{return;}},
                       Some(Event::Completed)|Some(Event::Failed(_))=>{
                        let response=simulations::result(state.clone(),user.clone(),run_id,false).await;
                        let message=match response {Ok(r)=>Server::SimulationStatus{run:r.run},Err(e)=>Server::Error{message:e.public_message()}};
                        if !send(&mut conn,message).await{return;}
                        active=None;
                       },
                       None=>{if !send(&mut conn,Server::Error{message:"simulation worker ended unexpectedly".into()}).await{return;}active=None;},
                      }
                     }
                    }
                } else {
                    match conn.recv().await {
                        Some(Ok(Client::Ping)) => {
                            if !send(&mut conn, Server::Heartbeat).await {
                                return;
                            }
                        }
                        Some(Ok(Client::Simulate { file_id, request })) => {
                            // Recheck server-side session expiry/revocation for every submitted job,
                            // not only the original WebSocket handshake.
                            let session = token.clone();
                            let current = state
                                .db(move |conn| {
                                    session_user(conn, &session)?.ok_or(ApiError::Unauthorized)
                                })
                                .await;
                            let result = match current {
                                Ok(user) => {
                                    simulations::begin(state.clone(), user, file_id, request).await
                                }
                                Err(e) => Err(e),
                            };
                            match result {
                                Ok(job) => {
                                    if !send(
                                        &mut conn,
                                        Server::SimulationStatus {
                                            run: job.run.clone(),
                                        },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                    active = Some(job);
                                }
                                Err(e) => {
                                    if !send(
                                        &mut conn,
                                        Server::Error {
                                            message: e.public_message(),
                                        },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        Some(Ok(Client::CancelSimulation { .. })) => {
                            if !send(
                                &mut conn,
                                Server::Error {
                                    message: "no active simulation".into(),
                                },
                            )
                            .await
                            {
                                return;
                            }
                        }
                        Some(Err(_)) => {
                            if !send(
                                &mut conn,
                                Server::Error {
                                    message: "invalid simulation message".into(),
                                },
                            )
                            .await
                            {
                                return;
                            }
                        }
                        None => return,
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{offline_state, test_config, TEST_PUBLIC_URL};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    #[tokio::test]
    async fn upgrades_reject_foreign_origins_and_missing_sessions() {
        let app = crate::build_app(offline_state(test_config()));
        for (origin, expected) in [
            ("https://hostile.example", StatusCode::FORBIDDEN),
            (TEST_PUBLIC_URL, StatusCode::UNAUTHORIZED),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get("/ws/simulations")
                        .header("origin", origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }
    #[tokio::test]
    async fn live_socket_streams_and_rechecks_revoked_sessions() {
        use crate::test_support::{db_app, sign_up, Client as HttpClient};
        use axum::http::Method;
        use diesel::prelude::*;
        use futures_util::{SinkExt, StreamExt};
        use serde_json::json;
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{client::IntoClientRequest, Message},
        };
        let Some((state, app)) = db_app() else {
            return;
        };
        let owner = sign_up(&state, "ws-owner");
        let http = HttpClient::new(&app, &owner);
        let org: shared::Organization = http
            .json(Method::POST, "/api/orgs", &json!({"name":"socket test"}))
            .await
            .json();
        let project: shared::Project = http
            .json(
                Method::POST,
                &format!("/api/orgs/{}/projects", org.id),
                &json!({"name":"socket"}),
            )
            .await
            .json();
        let file: shared::FileInfo = http
            .upload(
                project.id,
                "scalar.mdl",
                include_bytes!("../../../crates/unlinked-cli/tests/fixtures/scalar.mdl").to_vec(),
            )
            .await
            .json();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut request = format!("ws://{address}/ws/simulations")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("cookie", owner.cookie.parse().unwrap());
        request
            .headers_mut()
            .insert("origin", TEST_PUBLIC_URL.parse().unwrap());
        let (mut socket, _) = connect_async(request).await.unwrap();
        let start = Client::Simulate {
            file_id: file.id,
            request: shared::SimulationRequest {
                options: unlinked_sim::Options {
                    stop: 0.2,
                    step: 0.1,
                    ..Default::default()
                },
                workspace: Default::default(),
                inputs: Default::default(),
                version: None,
                init_script: None,
            },
        };
        socket
            .send(Message::Text(serde_json::to_string(&start).unwrap()))
            .await
            .unwrap();
        let mut count = 0;
        let mut named = false;
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let Message::Text(text) = message else {
                continue;
            };
            match serde_json::from_str::<Server>(&text).unwrap() {
                Server::SimulationStarted { signals, .. } => {
                    assert_eq!(signals.len(), 3);
                    named = true;
                }
                Server::SimulationSamples { time, values, .. } => {
                    assert!(named);
                    count += time.len();
                    assert_eq!(time.len(), values.len());
                }
                Server::SimulationStatus { run }
                    if run.status == shared::SimulationStatus::Completed =>
                {
                    break
                }
                Server::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(count, 3);
        diesel::delete(
            crate::schema::sessions::table
                .filter(crate::schema::sessions::user_id.eq(owner.user.id)),
        )
        .execute(&mut state.db_pool.get().unwrap())
        .unwrap();
        socket
            .send(Message::Text(serde_json::to_string(&start).unwrap()))
            .await
            .unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let error: Server = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert!(matches!(error, Server::Error { .. }));
        socket.close(None).await.unwrap();
        server.abort();
    }
}
