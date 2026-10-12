//! Open a `.slx`/`.mdl` file from disk and view it. Parsing and rendering
//! both happen in the browser; nothing is uploaded.

use crate::diagram::DiagramView;
use crate::Route;
use std::rc::Rc;
use unlinked_model::Model;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DragEvent, Event, HtmlInputElement};
use yew::prelude::*;
use yew_router::prelude::Link;

enum Load {
    Empty,
    Loading(String),
    Loaded(Rc<Model>),
    Failed(String),
}

/// Largest model file read into the browser, matching the CLI's limit.
const MAX_FILE_BYTES: f64 = 16.0 * 1024.0 * 1024.0;

/// Read and import `file`. `generation` identifies the latest request so a
/// slow earlier load cannot overwrite a newer one.
fn load_file(
    file: web_sys::File,
    state: UseStateHandle<Load>,
    generation: Rc<std::cell::Cell<u64>>,
    problem: UseStateHandle<Option<String>>,
) {
    let name = file.name();
    if !name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("slx") || extension.eq_ignore_ascii_case("mdl")
    }) {
        problem.set(Some(format!("{name}: choose a .slx or .mdl model file.")));
        return;
    }
    problem.set(None);
    let ticket = generation.get().wrapping_add(1);
    generation.set(ticket);
    if file.size() > MAX_FILE_BYTES {
        state.set(Load::Failed(format!(
            "{name}: larger than the 16 MiB limit"
        )));
        return;
    }
    state.set(Load::Loading(name.clone()));
    spawn_local(async move {
        let blob = gloo_file::File::from(file);
        let result = match gloo_file::futures::read_as_bytes(&blob).await {
            Ok(bytes) => unlinked_import::import(&name, &bytes)
                .map(|m| Load::Loaded(Rc::new(m)))
                .unwrap_or_else(|e| Load::Failed(format!("{name}: {e}"))),
            Err(e) => Load::Failed(format!("{name}: {e}")),
        };
        if generation.get() == ticket {
            state.set(result);
        }
    });
}

#[function_component(LocalViewer)]
pub fn local_viewer() -> Html {
    let state = use_state(|| Load::Empty);
    let dragging = use_state(|| false);
    let problem = use_state(|| None::<String>);
    let generation = use_memo((), |_| Rc::new(std::cell::Cell::new(0u64)));

    let onchange = {
        let state = state.clone();
        let problem = problem.clone();
        let generation = (*generation).clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            if let Some(file) = input.files().and_then(|f| f.get(0)) {
                load_file(file, state.clone(), generation.clone(), problem.clone());
            }
            // Selecting the same local file again should reload changed bytes.
            input.set_value("");
        })
    };
    let ondragover = {
        let dragging = dragging.clone();
        Callback::from(move |e: DragEvent| {
            if e.data_transfer()
                .is_some_and(|d| d.types().includes(&"Files".into(), 0))
            {
                e.prevent_default();
                dragging.set(true);
            }
        })
    };
    let ondragleave = {
        let dragging = dragging.clone();
        Callback::from(move |e: DragEvent| {
            let within = e
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|node| {
                    node.contains(
                        e.related_target()
                            .as_ref()
                            .and_then(|t| t.dyn_ref::<web_sys::Node>()),
                    )
                });
            if !within {
                dragging.set(false);
            }
        })
    };
    let ondrop = {
        let state = state.clone();
        let problem = problem.clone();
        let dragging = dragging.clone();
        let generation = (*generation).clone();
        Callback::from(move |e: DragEvent| {
            dragging.set(false);
            let Some(data) = e
                .data_transfer()
                .filter(|d| d.types().includes(&"Files".into(), 0))
            else {
                return;
            };
            // File drags must not navigate the browser, even if no readable
            // file item is available (for example a dropped directory).
            e.prevent_default();
            if let Some(file) = data.files().and_then(|f| f.get(0)) {
                load_file(file, state.clone(), generation.clone(), problem.clone());
            }
        })
    };

    let picker = html! {
        <label class="button file-picker">
            { "Choose a model…" }
            <input type="file" accept=".slx,.mdl" class="sr-only" {onchange} />
        </label>
    };

    let content = match &*state {
        Load::Loaded(model) => html! {
            <div class="page-fill">
                <div class="subbar file-bar">
                    { picker }
                    <span class="muted">{ "Local preview only. To simulate, upload this model to a project and select its Simulate tab." }</span>
                    <Link<Route> to={Route::Home} classes="button">{ "Open projects to simulate" }</Link<Route>>
                </div>
                <DiagramView model={model.clone()} />
            </div>
        },
        other => html! {
            <div class="dropzone">
                <h2>{ "Open a Simulink model" }</h2>
                <p class="muted">{ "Drop a .slx or .mdl file here. It is parsed and rendered in your browser and never leaves this machine." }</p>
                { picker }
                { match other {
                    Load::Loading(name) => html! { <p role="status">{ format!("Loading {name}…") }</p> },
                    Load::Failed(err) => html! { <p role="alert" class="error">{ err }</p> },
                    _ => html! {},
                } }
            </div>
        },
    };
    html! {
        <div class={classes!("page-fill", "local-viewer", dragging.then_some("over"))}
            {ondragover} {ondragleave} {ondrop}>
            <div role="alert" class="error local-file-error">{ problem.as_ref().cloned().unwrap_or_default() }</div>
            { content }
        </div>
    }
}
