//! Create CSV only on demand; release its Blob URL with the workspace.
use wasm_bindgen::JsCast;

pub(super) struct CsvDownload(String);
impl CsvDownload {
    pub fn save(csv: &str, filename: &str) -> Result<Self, String> {
        let error = |e| format!("Could not export CSV: {e:?}");
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
