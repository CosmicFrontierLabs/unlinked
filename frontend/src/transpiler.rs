use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

const EXAMPLE: &str =
    "A = [1 2; 3 4];\nx = [2; 1];\ny = A*x;\nfprintf('y = %g, %g\\n', y(1), y(2));\n";

#[function_component(Transpiler)]
pub fn transpiler() -> Html {
    let source = use_state(|| EXAMPLE.to_string());
    let library = use_state(|| false);
    let generated = use_state(|| None::<Result<String, String>>);
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
                (if *library {
                    unlinked_matlab::transpile_library(&source)
                } else {
                    unlinked_matlab::transpile(&source)
                })
                .map_err(|error| error.to_string())
            };
            generated.set(Some(result));
        })
    };
    html! {
        <div class="page">
            <h1>{"MATLAB to Rust"}</h1>
            <p>{"Translate a MATLAB or Octave script into standalone Rust in your browser. Source stays on this device."}</p>
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
                Some(Ok(code)) => {
                    let href = format!("data:text/plain;charset=utf-8,{}", js_sys::encode_uri_component(code).as_string().unwrap_or_default());
                    html! {
                        <section>
                            <h2>{"Generated Rust"}</h2>
                            <a class="button" href={href} download="generated.rs">{"Download generated.rs"}</a>
                            <p class="muted">{"For LLVM IR, use the command line: unlinked transpile source.m --emit llvm-ir -o output.ll"}</p>
                            <textarea aria-label="Generated Rust" rows="20" readonly=true spellcheck="false" value={code.clone()} style="width:100%;font-family:monospace;box-sizing:border-box" />
                        </section>
                    }
                }
            }}
        </div>
    }
}
