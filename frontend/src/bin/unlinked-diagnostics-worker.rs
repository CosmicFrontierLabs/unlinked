//! Classic dedicated worker loaded through Trunk's generated loader shim.
#[path = "../diagnostics_protocol.rs"]
pub mod protocol;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

fn send(scope: &DedicatedWorkerGlobalScope, response: protocol::Response) {
    if let Ok(text) = protocol::encode_response(&response) {
        if scope.post_message(&JsValue::from_str(&text)).is_ok() {
            return;
        }
    }
    web_sys::console::error_1(&"Diagnostics worker could not serialize/post its response.".into());
    scope.close();
}
fn main() {
    let scope = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();
    let target = scope.clone();
    let handler = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let result = event
            .data()
            .as_string()
            .ok_or_else(|| "Diagnostics request must be a JSON string.".to_string())
            .and_then(|text| protocol::decode_request(&text));
        let response = match result {
            Ok(request) => protocol::Response::Report {
                generation: request.generation,
                report: unlinked_sim::diagnose::diagnose(&request.model, &request.context),
            },
            Err(message) => protocol::Response::Error {
                generation: None,
                message,
            },
        };
        send(&target, response);
        target.close();
    });
    scope.set_onmessage(Some(handler.as_ref().unchecked_ref()));
    // The dedicated worker's lifetime owns this single callback. Terminating
    // the worker releases its complete WASM memory and the forgotten closure.
    handler.forget();
    send(
        &scope,
        protocol::Response::Ready {
            version: protocol::PROTOCOL_VERSION,
        },
    );
}
