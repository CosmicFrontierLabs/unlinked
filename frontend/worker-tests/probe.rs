//! Browser-test entrypoint only; not linked or exported by the application.
#[path = "../src/diagnostics_worker.rs"]
pub mod diagnostics_worker;
use diagnostics_worker::{DiagnosticOutcome, DiagnosticsWorker};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub struct WorkerProbe {
    worker: DiagnosticsWorker,
    events: Rc<RefCell<Vec<String>>>,
}
#[wasm_bindgen]
impl WorkerProbe {
    #[wasm_bindgen(constructor)]
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            worker: DiagnosticsWorker::new(timeout_ms),
            events: Default::default(),
        }
    }
    pub fn start(&mut self, request: &str) -> Result<(), JsValue> {
        let request = diagnostics_worker::protocol::decode_request(request)
            .map_err(|e| JsValue::from_str(&e))?;
        let events = self.events.clone();
        self.worker
            .start(
                request.generation,
                &request.model,
                &request.context,
                yew::Callback::from(move |outcome: DiagnosticOutcome| {
                    let value = match outcome.result {
                        Ok(report) => {
                            serde_json::json!({"generation":outcome.generation,"report":report})
                        }
                        Err(error) => {
                            serde_json::json!({"generation":outcome.generation,"error":error})
                        }
                    };
                    events.borrow_mut().push(value.to_string());
                }),
            )
            .map_err(|e| JsValue::from_str(&e))
    }
    pub fn events(&self) -> String {
        format!("[{}]", self.events.borrow().join(","))
    }
    pub fn cancel(&mut self) {
        self.worker.cancel();
    }
}
fn main() {}
