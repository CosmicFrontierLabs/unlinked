//! Disposable off-main-thread diagnostics. Keep this handle in a component ref;
//! call `cancel` whenever its model/context becomes stale and let Drop cancel on
//! unmount. `start` cancels the previous request before validating the new one.
//! The callback runs once on completion/error/timeout, never on explicit cancel.
#[path = "diagnostics_protocol.rs"]
pub mod protocol;
use gloo_events::EventListener;
use gloo_timers::callback::Timeout;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use unlinked_model::Model;
use unlinked_sim::diagnose::{DiagnosticContext, DiagnosticReport};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{ErrorEvent, MessageEvent, Worker};
use yew::Callback;

pub const DEFAULT_TIMEOUT_MS: u32 = 15_000;
const LOADER: &str = "unlinked-diagnostics-worker_loader.js";
#[derive(Debug)]
pub struct DiagnosticOutcome {
    pub generation: u64,
    pub result: Result<DiagnosticReport, String>,
}
struct Job {
    worker: Worker,
    generation: u64,
    current: Rc<Cell<u64>>,
    token: u64,
    active: Cell<bool>,
    pending: RefCell<Option<String>>,
    callback: Callback<DiagnosticOutcome>,
}
impl Job {
    fn live(&self) -> bool {
        self.active.get() && self.current.get() == self.token
    }
    fn finish(&self, result: Result<DiagnosticReport, String>) {
        if !self.live() {
            return;
        }
        self.active.set(false);
        self.pending.borrow_mut().take();
        self.worker.terminate();
        // No RefCell borrow spans external code, which may cancel/drop/restart.
        self.callback.emit(DiagnosticOutcome {
            generation: self.generation,
            result,
        });
    }
    fn message(&self, event: &MessageEvent) {
        if !self.live() {
            return;
        }
        let Some(text) = event.data().as_string() else {
            self.finish(Err("Diagnostics worker sent a non-text response.".into()));
            return;
        };
        if text.len() > 4 * 1024 * 1024 {
            self.finish(Err("Diagnostics worker response exceeds 4 MiB.".into()));
            return;
        }
        let response = match serde_json::from_str::<protocol::Response>(&text) {
            Ok(response) => response,
            Err(e) => {
                self.finish(Err(format!("Invalid diagnostics worker response: {e}")));
                return;
            }
        };
        match response {
            protocol::Response::Ready { version } => {
                if version != protocol::PROTOCOL_VERSION {
                    self.finish(Err(
                        "Diagnostics worker protocol version mismatch; reload the page.".into(),
                    ));
                    return;
                }
                let pending = self.pending.borrow_mut().take();
                if let Some(request) = pending {
                    if let Err(e) = self.worker.post_message(&JsValue::from_str(&request)) {
                        self.finish(Err(format!("Cannot post diagnostics request: {e:?}")));
                    }
                }
            }
            protocol::Response::Report { generation, report } => {
                if generation == self.generation && self.pending.borrow().is_none() {
                    self.finish(Ok(report));
                }
            }
            protocol::Response::Error {
                generation,
                message,
            } => {
                if generation.is_none_or(|g| g == self.generation) {
                    self.finish(Err(message));
                }
            }
        }
    }
}
struct Running {
    job: Rc<Job>,
    _listeners: Vec<EventListener>,
    _timeout: Timeout,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.job.active.set(false);
        self.job.worker.terminate();
    }
}
/// Owns one request at a time. Each start creates a fresh worker, isolating even
/// noncooperative compiler work. A private token also guards reused generations.
pub struct DiagnosticsWorker {
    current: Rc<Cell<u64>>,
    running: Option<Running>,
    timeout_ms: u32,
}
impl Default for DiagnosticsWorker {
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT_MS)
    }
}
impl DiagnosticsWorker {
    pub fn new(timeout_ms: u32) -> Self {
        Self {
            current: Rc::new(Cell::new(0)),
            running: None,
            timeout_ms: timeout_ms.max(1),
        }
    }
    /// Cancel is silent; the panel owns the decision to clear or retain old results.
    pub fn cancel(&mut self) {
        self.current.set(self.current.get().wrapping_add(1));
        self.running.take();
    }
    pub fn start(
        &mut self,
        generation: u64,
        model: &Model,
        context: &DiagnosticContext,
        callback: Callback<DiagnosticOutcome>,
    ) -> Result<(), String> {
        self.cancel();
        let request = protocol::encode_request(generation, model, context)?;
        let base = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.base_uri().ok().flatten())
            .ok_or("Cannot resolve diagnostics worker URL.")?;
        // Trunk's worker assets deliberately retain the binary name. The base
        // element/public URL also supports deployment below a path prefix.
        let url = web_sys::Url::new_with_base(LOADER, &base)
            .map_err(|e| format!("Invalid diagnostics worker URL: {e:?}"))?;
        let worker = Worker::new(&url.href())
            .map_err(|e| format!("Cannot create diagnostics worker: {e:?}"))?;
        let job = Rc::new(Job {
            worker,
            generation,
            current: self.current.clone(),
            token: self.current.get(),
            active: Cell::new(true),
            pending: RefCell::new(Some(request)),
            callback,
        });
        let listener_job = job.clone();
        let message = EventListener::new(&job.worker, "message", move |event| {
            let job = listener_job.clone();
            if let Some(event) = event.dyn_ref::<MessageEvent>() {
                job.message(event);
            }
        });
        let listener_job = job.clone();
        let error = EventListener::new(&job.worker, "error", move |event| {
            let job = listener_job.clone();
            let detail = event
                .dyn_ref::<ErrorEvent>()
                .map(|e| e.message())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "worker bootstrap or execution failed".into());
            job.finish(Err(format!("Diagnostics worker error: {detail}")));
        });
        let listener_job = job.clone();
        let message_error = EventListener::new(&job.worker, "messageerror", move |_| {
            let job = listener_job.clone();
            job.finish(Err("Cannot decode diagnostics worker message.".into()));
        });
        let timeout_job = Rc::downgrade(&job);
        let timeout = Timeout::new(self.timeout_ms, move || {
            if let Some(job) = timeout_job.upgrade() {
                job.finish(Err(
                    "Diagnostics timed out; its worker was terminated.".into()
                ));
            }
        });
        self.running = Some(Running {
            job,
            _listeners: vec![message, error, message_error],
            _timeout: timeout,
        });
        Ok(())
    }
}
impl Drop for DiagnosticsWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}
