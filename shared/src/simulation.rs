//! Shared simulation requests and version-pinned run metadata.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use unlinked_sim::{Options as SimulationOptions, Solver, Trace as SimulationTrace};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationInitScript {
    pub file_id: Uuid,
    #[serde(default)]
    pub version: Option<i32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationRequest {
    pub options: SimulationOptions,
    #[serde(default)]
    pub workspace: BTreeMap<String, String>,
    /// Constant root Inport expressions, keyed by original block ID.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// Omit to select the latest immutable version at submission time.
    #[serde(default)]
    pub version: Option<i32>,
    #[serde(default)]
    pub init_script: Option<SimulationInitScript>,
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
    fn stream_messages_roundtrip() {
        let message = SimulationServerMsg::SimulationSamples {
            run_id: Uuid::new_v4(),
            time: vec![0.0, 0.1],
            values: vec![vec![1.0, 2.0], vec![3.0, 4.0]],
        };
        let serialized = serde_json::to_string(&message).unwrap();
        let decoded: SimulationServerMsg = serde_json::from_str(&serialized).unwrap();
        match decoded {
            SimulationServerMsg::SimulationSamples { time, values, .. } => {
                assert_eq!(time.len(), 2);
                assert_eq!(values[1], vec![3.0, 4.0]);
            }
            _ => panic!("wrong variant"),
        }
        let request = SimulationClientMsg::Simulate {
            file_id: Uuid::new_v4(),
            request: SimulationRequest {
                options: Default::default(),
                workspace: Default::default(),
                inputs: Default::default(),
                version: None,
                init_script: None,
            },
        };
        let decoded: SimulationClientMsg =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert!(matches!(decoded, SimulationClientMsg::Simulate { .. }));
    }
    #[test]
    fn init_script_reference_roundtrip_and_legacy_default() {
        let file_id = Uuid::new_v4();
        let script = SimulationInitScript {
            file_id,
            version: Some(7),
        };
        let encoded = serde_json::to_string(&script).unwrap();
        let decoded: SimulationInitScript = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.file_id, file_id);
        assert_eq!(decoded.version, Some(7));
        let legacy: SimulationRequest =
            serde_json::from_str(r#"{"options":{},"workspace":{},"version":1}"#).unwrap();
        assert!(legacy.init_script.is_none());
        assert!(legacy.inputs.is_empty());
    }
    #[test]
    fn request_and_trace_roundtrip() {
        let request = SimulationRequest {
            options: SimulationOptions::default(),
            workspace: BTreeMap::from([("gain".into(), "2*pi".into())]),
            inputs: BTreeMap::from([("9".into(), "gain/2".into())]),
            version: Some(3),
            init_script: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: SimulationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workspace["gain"], "2*pi");
        assert_eq!(back.inputs["9"], "gain/2");
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
