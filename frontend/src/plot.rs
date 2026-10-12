//! Binding rizzma figures to a canvas. rizzma's interactive canvas session
//! exists only on wasm32; native builds (workspace checks and tests) get an
//! inert stand-in with the same interface.

use rizzma::wasm::WasmFigure;
use wasm_bindgen::JsValue;

#[cfg(target_arch = "wasm32")]
pub use rizzma::wasm::WasmSession as PlotSession;

#[cfg(not(target_arch = "wasm32"))]
pub struct PlotSession;

#[cfg(not(target_arch = "wasm32"))]
impl PlotSession {
    pub fn limits(&self, _axes: usize) -> Result<Box<[f64]>, JsValue> {
        Ok(Box::new([0.0, 1.0, 0.0, 1.0]))
    }
    pub fn set_line_data(
        &self,
        _axes: usize,
        _line: usize,
        _x: &[f64],
        _y: &[f64],
    ) -> Result<(), JsValue> {
        Ok(())
    }
}

/// Attach `fig` to the canvas with id `canvas_id` as an interactive session.
pub fn bind(fig: WasmFigure, canvas_id: &str) -> Result<PlotSession, JsValue> {
    #[cfg(target_arch = "wasm32")]
    {
        fig.bind(canvas_id)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (fig, canvas_id);
        Ok(PlotSession)
    }
}

/// Observe the plot's content width, including pane and history-panel changes.
#[yew::hook]
pub fn use_plot_width(host: yew::NodeRef) -> u32 {
    use wasm_bindgen::{closure::Closure, JsCast};
    use yew::prelude::*;
    let width = use_state_eq(|| 0u32);
    {
        let width = width.clone();
        use_effect_with(host, move |host| {
            let callback =
                Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
                    if let Some(entry) = entries.get(0).dyn_ref::<web_sys::ResizeObserverEntry>() {
                        let measured = entry.content_rect().width().round() as u32;
                        if measured > 0 {
                            width.set(measured);
                        }
                    }
                });
            let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok();
            if let (Some(observer), Some(element)) = (&observer, host.cast::<web_sys::Element>()) {
                observer.observe(&element);
            }
            move || {
                if let Some(observer) = observer {
                    observer.disconnect();
                }
                drop(callback);
            }
        });
    }
    *width
}
