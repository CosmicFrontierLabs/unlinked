//! Open a `.slx`/`.mdl` file from disk and view it. Parsing and rendering
//! both happen in the browser; nothing is uploaded.

use crate::diagram::DiagramView;
use std::rc::Rc;
use unlinked_model::Model;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DragEvent, Event, HtmlInputElement};
use yew::prelude::*;

enum Load {
    Empty,
    Loading(String),
    Loaded(Rc<Model>),
    Failed(String),
}

fn load_file(file: web_sys::File, state: UseStateHandle<Load>) {
    let name = file.name();
    state.set(Load::Loading(name.clone()));
    spawn_local(async move {
        let blob = gloo_file::File::from(file);
        match gloo_file::futures::read_as_bytes(&blob).await {
            Ok(bytes) => match unlinked_import::import(&name, &bytes) {
                Ok(model) => state.set(Load::Loaded(Rc::new(model))),
                Err(e) => state.set(Load::Failed(format!("{name}: {e}"))),
            },
            Err(e) => state.set(Load::Failed(format!("{name}: {e}"))),
        }
    });
}

#[function_component(LocalViewer)]
pub fn local_viewer() -> Html {
    let state = use_state(|| Load::Empty);
    let dragging = use_state(|| false);

    let onchange = {
        let state = state.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            if let Some(file) = input.files().and_then(|f| f.get(0)) {
                load_file(file, state.clone());
            }
        })
    };
    let ondragover = {
        let dragging = dragging.clone();
        Callback::from(move |e: DragEvent| {
            e.prevent_default();
            dragging.set(true);
        })
    };
    let ondragleave = {
        let dragging = dragging.clone();
        Callback::from(move |_: DragEvent| dragging.set(false))
    };
    let ondrop = {
        let state = state.clone();
        let dragging = dragging.clone();
        Callback::from(move |e: DragEvent| {
            e.prevent_default();
            dragging.set(false);
            if let Some(file) = e
                .data_transfer()
                .and_then(|d| d.files())
                .and_then(|f| f.get(0))
            {
                load_file(file, state.clone());
            }
        })
    };

    let picker = html! {
        <label class="button">
            { "Choose a model…" }
            <input type="file" accept=".slx,.mdl" hidden=true {onchange} />
        </label>
    };

    match &*state {
        Load::Loaded(model) => html! {
            <div class="page-fill">
                <div class="subbar">{ picker }</div>
                <DiagramView model={model.clone()} />
            </div>
        },
        other => html! {
            <div class={classes!("dropzone", dragging.then_some("over"))}
                {ondragover} {ondragleave} {ondrop}>
                <h2>{ "Open a Simulink model" }</h2>
                <p class="muted">{ "Drop a .slx or .mdl file here. It is parsed and rendered in your browser and never leaves this machine." }</p>
                { picker }
                { match other {
                    Load::Loading(name) => html! { <p>{ format!("Loading {name}…") }</p> },
                    Load::Failed(err) => html! { <p class="error">{ err }</p> },
                    _ => html! {},
                } }
            </div>
        },
    }
}
