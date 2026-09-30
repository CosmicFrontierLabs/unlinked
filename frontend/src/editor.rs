//! Edit a stored model and save the result as a new file version.
//!
//! Edits are applied to a working copy of the IR for immediate feedback and
//! recorded; saving replays them onto the original file bytes with
//! `unlinked_import::patch`, so everything the IR does not model survives.

use crate::api;
use crate::diagram::DiagramView;
use std::rc::Rc;
use unlinked_model::edit::Edit;
use unlinked_model::Model;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, InputEvent};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct EditorProps {
    pub project_id: Uuid,
    /// The file's path in the project; also used to detect the format.
    pub path: String,
    pub model: Rc<Model>,
    pub bytes: Rc<Vec<u8>>,
    pub can_edit: bool,
    /// Stable identity of the shown version, so edits don't re-fit the view.
    pub fit_key: AttrValue,
    /// Called after a new version has been saved.
    pub on_saved: Callback<()>,
}

/// Replay `edits` onto `original`.
fn replay(original: &Model, edits: &[Edit]) -> Result<Model, String> {
    let mut m = original.clone();
    for e in edits {
        e.apply(&mut m).map_err(|e| e.to_string())?;
    }
    Ok(m)
}

#[function_component(ModelEditor)]
pub fn model_editor(props: &EditorProps) -> Html {
    let editing = use_state(|| false);
    let pending = use_state(Vec::<Edit>::new);
    let working = use_state(|| props.model.clone());
    let error = use_state(|| None::<String>);
    let message = use_state(String::new);
    let saving = use_state(|| false);

    let on_edit = {
        let (pending, working, error) = (pending.clone(), working.clone(), error.clone());
        Callback::from(move |edit: Edit| {
            let mut next = (**working).clone();
            match edit.apply(&mut next) {
                Ok(()) => {
                    let mut p = (*pending).clone();
                    p.push(edit);
                    pending.set(p);
                    working.set(Rc::new(next));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        })
    };

    let start = {
        let editing = editing.clone();
        Callback::from(move |_: MouseEvent| editing.set(true))
    };
    let discard = {
        let (editing, pending, working, error, model) = (
            editing.clone(),
            pending.clone(),
            working.clone(),
            error.clone(),
            props.model.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            editing.set(false);
            pending.set(Vec::new());
            working.set(model.clone());
            error.set(None);
        })
    };
    let undo = {
        let (pending, working, error, model) = (
            pending.clone(),
            working.clone(),
            error.clone(),
            props.model.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            let mut p = (*pending).clone();
            p.pop();
            match replay(&model, &p) {
                Ok(m) => {
                    working.set(Rc::new(m));
                    pending.set(p);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
        })
    };
    let save = {
        let (pending, message, saving, error, editing, working) = (
            pending.clone(),
            message.clone(),
            saving.clone(),
            error.clone(),
            editing.clone(),
            working.clone(),
        );
        let (project_id, path, bytes, on_saved, model) = (
            props.project_id,
            props.path.clone(),
            props.bytes.clone(),
            props.on_saved.clone(),
            props.model.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            let edits = (*pending).clone();
            if edits.is_empty() {
                return;
            }
            let patched = match unlinked_import::patch::apply_edits(&path, &bytes, &edits) {
                Ok(b) => b,
                Err(e) => {
                    error.set(Some(e.to_string()));
                    return;
                }
            };
            let msg = if message.trim().is_empty() {
                format!(
                    "{} edit{}",
                    edits.len(),
                    if edits.len() == 1 { "" } else { "s" }
                )
            } else {
                message.trim().to_string()
            };
            saving.set(true);
            let (path, saving, error, editing, pending, working, on_saved, model, message) = (
                path.clone(),
                saving.clone(),
                error.clone(),
                editing.clone(),
                pending.clone(),
                working.clone(),
                on_saved.clone(),
                model.clone(),
                message.clone(),
            );
            spawn_local(async move {
                let result = api::upload(project_id, &path, &msg, &patched).await;
                saving.set(false);
                match result {
                    Ok(_) => {
                        editing.set(false);
                        pending.set(Vec::new());
                        working.set(model);
                        message.set(String::new());
                        error.set(None);
                        on_saved.emit(());
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
            });
        })
    };
    let set_message = {
        let message = message.clone();
        Callback::from(move |e: InputEvent| {
            message.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };

    let count = pending.len();
    let toolbar = if !props.can_edit {
        html! {}
    } else if !*editing {
        html! {
            <div class="edit-bar">
                <button onclick={start}>{ "Edit" }</button>
            </div>
        }
    } else {
        html! {
            <div class="edit-bar editing">
                <strong>{ "Editing" }</strong>
                <span class="muted">{ "Drag blocks, edit names and parameters in the inspector, Delete removes the selected block." }</span>
                <span class="spacer" />
                <span>{ format!("{count} change{}", if count == 1 { "" } else { "s" }) }</span>
                <button onclick={undo} disabled={count == 0 || *saving}>{ "Undo" }</button>
                <button onclick={discard} disabled={*saving}>{ "Discard" }</button>
                <input placeholder="Change message" value={(*message).clone()} oninput={set_message} disabled={*saving} />
                <button class="primary" onclick={save} disabled={count == 0 || *saving}>
                    { if *saving { "Saving…" } else { "Save as new version" } }
                </button>
            </div>
        }
    };

    let model = if *editing {
        (*working).clone()
    } else {
        props.model.clone()
    };
    html! {
        <>
            { toolbar }
            if let Some(e) = &*error {
                <div class="edit-bar error">{ e }</div>
            }
            <DiagramView {model} fit_key={props.fit_key.clone()}
                on_edit={(*editing && !*saving).then(|| on_edit.clone())} />
        </>
    }
}
