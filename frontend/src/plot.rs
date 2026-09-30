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
