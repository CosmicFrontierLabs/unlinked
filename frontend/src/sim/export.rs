//! Download recorded data or the current plot on demand.
use wasm_bindgen::{JsCast, JsValue};

fn failure(kind: &str, value: JsValue) -> String {
    // Debug-formatting a JS error can include its entire WASM stack. Keep
    // the actionable message in the UI so the user can still retry.
    let message = js_sys::Reflect::get(&value, &"message".into())
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| value.as_string())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| "The browser could not create the download.".into());
    let message: String = message.chars().take(240).collect();
    format!("Could not export {kind}: {message}")
}

pub(super) struct CsvDownload(String);
impl CsvDownload {
    pub fn save(csv: &str, filename: &str) -> Result<Self, String> {
        let error = |e| failure("CSV", e);
        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(csv));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("text/csv;charset=utf-8");
        let blob =
            web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).map_err(error)?;
        let url = Self(web_sys::Url::create_object_url_with_blob(&blob).map_err(error)?);
        let anchor = gloo_utils::document().create_element("a").map_err(error)?;
        anchor.set_attribute("href", &url.0).map_err(error)?;
        anchor.set_attribute("download", filename).map_err(error)?;
        anchor
            .dyn_into::<web_sys::HtmlElement>()
            .map_err(|e| error(e.into()))?
            .click();
        Ok(url)
    }
}
impl Drop for CsvDownload {
    fn drop(&mut self) {
        let _ = web_sys::Url::revoke_object_url(&self.0);
    }
}

/// Save exactly the visible canvas, retaining its current zoom and overlays.
pub(super) fn save_png(canvas_id: &str, filename: &str) -> Result<(), String> {
    let error = |e| failure("plot", e);
    let canvas = gloo_utils::document()
        .get_element_by_id(canvas_id)
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .ok_or_else(|| "Could not export plot: no plot is available.".to_string())?;
    let image = canvas.to_data_url_with_type("image/png").map_err(error)?;
    if !image.starts_with("data:image/png;") {
        return Err("Could not export plot: the plot has no image yet.".into());
    }
    let anchor = gloo_utils::document().create_element("a").map_err(error)?;
    anchor.set_attribute("href", &image).map_err(error)?;
    anchor.set_attribute("download", filename).map_err(error)?;
    anchor
        .dyn_into::<web_sys::HtmlElement>()
        .map_err(|e| error(e.into()))?
        .click();
    Ok(())
}
