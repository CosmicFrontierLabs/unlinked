use std::io::{Cursor, Write};
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

const EXAMPLE: &str =
    "A = [1 2; 3 4];\nx = [2; 1];\ny = A*x;\nfprintf('y = %g, %g\\n', y(1), y(2));\n";

#[derive(Properties, PartialEq)]
pub struct TranspilerProps {
    /// Initial source, e.g. a project `.m` file; an example otherwise.
    #[prop_or_default]
    pub source: Option<AttrValue>,
}

#[function_component(Transpiler)]
pub fn transpiler(props: &TranspilerProps) -> Html {
    let source = use_state(|| {
        props
            .source
            .as_ref()
            .map_or_else(|| EXAMPLE.to_string(), |s| s.to_string())
    });
    let library = use_state(|| false);
    let generated = use_state(|| {
        None::<Result<(unlinked_matlab::GeneratedProject, ObjectUrl, ObjectUrl), String>>
    });
    let edit = {
        let source = source.clone();
        let generated = generated.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlTextAreaElement = event.target_unchecked_into();
            source.set(input.value());
            generated.set(None);
        })
    };
    let mode = {
        let library = library.clone();
        let generated = generated.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            library.set(input.checked());
            generated.set(None);
        })
    };
    let compile = {
        let source = source.clone();
        let library = library.clone();
        let generated = generated.clone();
        Callback::from(move |_| {
            let result = if source.len() > 65_536 {
                Err("Source exceeds the 64 KiB limit".into())
            } else {
                unlinked_matlab::generate_project(&source, *library)
                    .map_err(|error| error.to_string())
                    .and_then(|project| {
                        let zip = project_zip(&project)?;
                        let archive = ObjectUrl::new(&zip, "application/zip")?;
                        let source_url = ObjectUrl::new(project.source.as_bytes(), "text/plain")?;
                        Ok((project, archive, source_url))
                    })
            };
            generated.set(Some(result));
        })
    };
    html! {
        <div class="page">
            <h1>{"MATLAB to Rust"}</h1>
            <p>{"Translate MATLAB or Octave into a portable Rust Cargo project using ndarray and nalgebra. Source stays on this device."}</p>
            <p class="muted">{"Supports a bounded subset of real matrices, indexing, functions and control flow. Unsupported syntax produces a diagnostic. Generated code is not executed here."}</p>
            <label for="matlab-source">{"MATLAB / Octave source"}</label>
            <textarea id="matlab-source" rows="16" maxlength="65536" spellcheck="false" value={(*source).clone()} oninput={edit} style="width:100%;font-family:monospace;box-sizing:border-box" />
            <div class="actions">
                <label><input type="checkbox" checked={*library} onchange={mode} />{" Export function library"}</label>
                <button class="primary" onclick={compile}>{"Generate Rust"}</button>
            </div>
            {match &*generated {
                None => html! {},
                Some(Err(error)) => html! {<p role="alert" class="error">{error}</p>},
                Some(Ok((project, archive, source_url))) => {
                    let code = &project.source;
                    html! {
                        <section>
                            <h2>{"Generated Rust"}</h2>
                            <a class="button" href={archive.0.clone()} download="generated-matlab.zip">{"Download Cargo project"}</a>
                            <a class="button" href={source_url.0.clone()} download={if project.library { "lib.rs" } else { "main.rs" }}>{"Download Rust source"}</a>
                            <p class="muted">{"For LLVM IR, use the command line: unlinked transpile source.m --emit llvm-ir -o output.ll"}</p>
                            <textarea aria-label="Generated Rust" rows="20" readonly=true spellcheck="false" value={code.clone()} style="width:100%;font-family:monospace;box-sizing:border-box" />
                            <h2>{"Cargo.toml"}</h2>
                            <p class="muted">{"The ZIP includes the MATLAB semantics helper crate. Extract it and run cargo build; scripts can then be run with cargo run. Function libraries expose typed Rust functions. Unsupported dynamic types produce diagnostics."}</p>
                            <textarea aria-label="Cargo.toml" rows="12" readonly=true spellcheck="false" value={project.manifest.clone()} style="width:100%;font-family:monospace;box-sizing:border-box" />
                        </section>
                    }
                }
            }}
        </div>
    }
}

fn project_zip(project: &unlinked_matlab::GeneratedProject) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, content) in project.files() {
        zip.start_file(format!("generated-matlab/{path}"), options)
            .map_err(|e| e.to_string())?;
        zip.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
}

/// Revoke generated download URLs when replaced, cleared, or the page unmounts.
struct ObjectUrl(String);
impl ObjectUrl {
    fn new(bytes: &[u8], mime: &str) -> Result<Self, String> {
        let parts = js_sys::Array::new();
        parts.push(&js_sys::Uint8Array::from(bytes));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(mime);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
            .map_err(|e| format!("Cannot create download: {e:?}"))?;
        web_sys::Url::create_object_url_with_blob(&blob)
            .map(Self)
            .map_err(|e| format!("Cannot create download URL: {e:?}"))
    }
}
impl Drop for ObjectUrl {
    fn drop(&mut self) {
        let _ = web_sys::Url::revoke_object_url(&self.0);
    }
}
