//! Shared simulation requests and version-pinned run metadata.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use unlinked_sim::{Options as SimulationOptions, Solver, Trace as SimulationTrace};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationRequest {
    pub options: SimulationOptions,
    #[serde(default)]
    pub workspace: BTreeMap<String, String>,
    /// Omit to select the latest immutable version at submission time.
    #[serde(default)]
    pub version: Option<i32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulationRun {
    pub id: Uuid,
    pub file_id: Uuid,
    pub file_version_id: Uuid,
    pub file_version: i32,
    pub requested_by: Option<Uuid>,
    pub status: SimulationStatus,
    pub request: SimulationRequest,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulationSignal {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulationResult {
    pub run: SimulationRun,
    pub trace: Option<SimulationTrace>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_and_trace_roundtrip() {
        let request = SimulationRequest {
            options: SimulationOptions::default(),
            workspace: BTreeMap::from([("gain".into(), "2*pi".into())]),
            version: Some(3),
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: SimulationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workspace["gain"], "2*pi");
        assert_eq!(back.version, Some(3));
        assert_eq!(back.options.solver, Solver::Rk4);
        let trace = SimulationTrace {
            time: vec![0.0],
            signals: BTreeMap::from([("1".into(), vec![2.0])]),
            solver: Solver::Euler,
        };
        let back: SimulationTrace =
            serde_json::from_str(&serde_json::to_string(&trace).unwrap()).unwrap();
        assert_eq!(back.signals["1"], vec![2.0]);
    }
}

pub struct SimulationSocket;
impl ws_bridge::WsEndpoint for SimulationSocket {
    const PATH: &'static str = "/ws/simulations";
    type ServerMsg = SimulationServerMsg;
    type ClientMsg = SimulationClientMsg;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SimulationClientMsg {
    Ping,
    Simulate {
        file_id: Uuid,
        request: SimulationRequest,
    },
    CancelSimulation {
        run_id: Uuid,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SimulationServerMsg {
    Heartbeat,
    Error {
        message: String,
    },
    SimulationStatus {
        run: SimulationRun,
    },
    SimulationStarted {
        run_id: Uuid,
        signals: Vec<SimulationSignal>,
    },
    /// One row per time, one column per signal from SimulationStarted.
    SimulationSamples {
        run_id: Uuid,
        time: Vec<f64>,
        values: Vec<Vec<f64>>,
    },
}
