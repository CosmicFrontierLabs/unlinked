//! Edit a stored model and save the result as a new file version.
//!
//! Edits are recorded as a batch against a pinned base version. The batch
//! is applied to a copy of the base IR for immediate feedback, and saving
//! replays it onto the base file bytes with `unlinked_import::patch`, so
//! everything the IR does not model survives. A save is refused when the
//! file has a newer version than the base; the user can then explicitly
//! reapply the batch on top of the latest version.

use crate::api;
use crate::diagram::DiagramView;
use std::rc::Rc;
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::Model;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, InputEvent};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct EditorProps {
    pub project_id: Uuid,
    pub file_id: Uuid,
    /// The version shown, which edits start from.
    pub base: Base,
    pub can_edit: bool,
    /// Stable identity of the shown version, so edits don't re-fit the view.
    pub fit_key: AttrValue,
    /// Called after a new version has been saved.
    pub on_saved: Callback<()>,
}

/// A file version edits apply to.
#[derive(Clone, PartialEq)]
pub struct Base {
    /// The file's path at this version; saves go here, and it also tells
    /// the patcher the format. Refreshed with the base, since the file may
    /// have been renamed.
    pub path: String,
    pub version_id: Uuid,
    pub version: i32,
    pub model: Rc<Model>,
    pub bytes: Rc<Vec<u8>>,
}

/// Replay `edits` onto `base`.
fn replay(base: &Model, edits: &[Edit]) -> Result<Model, String> {
    let mut model = base.clone();
    apply_batch(&mut model, edits).map_err(|e| e.to_string())?;
    Ok(model)
}

/// Load the file's latest version as a new base.
async fn latest_base(project_id: Uuid, file_id: Uuid) -> Result<Base, String> {
    let info = api::file(project_id, file_id)
        .await
        .map_err(|e| e.to_string())?;
    let bytes = api::content(project_id, file_id, Some(info.latest.id))
        .await
        .map_err(|e| e.to_string())?;
    let model = unlinked_import::import(&info.path, &bytes).map_err(|e| e.to_string())?;
    Ok(Base {
        path: info.path,
        version_id: info.latest.id,
        version: info.latest.version,
        model: Rc::new(model),
        bytes: Rc::new(bytes),
    })
}

#[function_component(ModelEditor)]
pub fn model_editor(props: &EditorProps) -> Html {
    let editing = use_state(|| false);
    let base = use_state(|| props.base.clone());
    let pending = use_state(Vec::<Edit>::new);
    let working = use_state(|| props.base.model.clone());
    let error = use_state(|| None::<String>);
    // Why the last save was refused because the file changed meanwhile.
    // Kept apart from `error`, which later edits clear, so the recovery
    // action stays visible until the user reapplies or discards.
    let stale = use_state(|| None::<String>);
    let message = use_state(String::new);
    let busy = use_state(|| false);
    // Cleared on unmount so a request finishing afterwards neither updates
    // state nor navigates.
    let mounted = use_mut_ref(|| true);
    {
        let mounted = mounted.clone();
        use_effect_with((), move |_| move || *mounted.borrow_mut() = false);
    }

    let on_edit = {
        let (pending, working, error) = (pending.clone(), working.clone(), error.clone());
        Callback::from(move |edit: Edit| {
            let mut next = (**working).clone();
            match apply_batch(&mut next, std::slice::from_ref(&edit)) {
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
        let (editing, pending, working, error, stale, base) = (
            editing.clone(),
            pending.clone(),
            working.clone(),
            error.clone(),
            stale.clone(),
            base.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            editing.set(false);
            pending.set(Vec::new());
            working.set(base.model.clone());
            error.set(None);
            stale.set(None);
        })
    };
    let undo = {
        let (pending, working, error, base) = (
            pending.clone(),
            working.clone(),
            error.clone(),
            base.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            let mut p = (*pending).clone();
            p.pop();
            match replay(&base.model, &p) {
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
        let (mounted, pending, message, busy, error, stale, editing, working, base) = (
            mounted.clone(),
            pending.clone(),
            message.clone(),
            busy.clone(),
            error.clone(),
            stale.clone(),
            editing.clone(),
            working.clone(),
            base.clone(),
        );
        let (project_id, on_saved) = (props.project_id, props.on_saved.clone());
        Callback::from(move |_: MouseEvent| {
            let edits = (*pending).clone();
            if edits.is_empty() {
                return;
            }
            let patched = match unlinked_import::patch::apply_edits(&base.path, &base.bytes, &edits)
            {
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
            busy.set(true);
            let (mounted, busy, error, stale, editing, pending, working, on_saved, message) = (
                mounted.clone(),
                busy.clone(),
                error.clone(),
                stale.clone(),
                editing.clone(),
                pending.clone(),
                working.clone(),
                on_saved.clone(),
                message.clone(),
            );
            let base = (*base).clone();
            spawn_local(async move {
                let result = api::upload(
                    project_id,
                    &base.path,
                    &msg,
                    &patched,
                    Some(base.version_id),
                )
                .await;
                if !*mounted.borrow() {
                    return;
                }
                busy.set(false);
                match result {
                    Ok(_) => {
                        editing.set(false);
                        pending.set(Vec::new());
                        working.set(base.model.clone());
                        message.set(String::new());
                        error.set(None);
                        on_saved.emit(());
                    }
                    Err(e) if e.status == 409 => {
                        stale.set(Some(e.message));
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
            });
        })
    };
    let reapply = {
        let (mounted, pending, busy, error, stale, working, base) = (
            mounted.clone(),
            pending.clone(),
            busy.clone(),
            error.clone(),
            stale.clone(),
            working.clone(),
            base.clone(),
        );
        let (project_id, file_id) = (props.project_id, props.file_id);
        Callback::from(move |_: MouseEvent| {
            busy.set(true);
            let (mounted, pending, busy, error, stale, working, base) = (
                mounted.clone(),
                pending.clone(),
                busy.clone(),
                error.clone(),
                stale.clone(),
                working.clone(),
                base.clone(),
            );
            spawn_local(async move {
                let latest = latest_base(project_id, file_id).await;
                if !*mounted.borrow() {
                    return;
                }
                busy.set(false);
                let latest = match latest {
                    Ok(l) => l,
                    Err(e) => return error.set(Some(e)),
                };
                // The edits keep their IDs from the old base; they only
                // carry over if every one still applies to the new version.
                match replay(&latest.model, &pending) {
                    Ok(model) => {
                        error.set(Some(format!(
                            "Your {} edit{} now apply on top of v{}. Review, then save.",
                            pending.len(),
                            if pending.len() == 1 { "" } else { "s" },
                            latest.version
                        )));
                        working.set(Rc::new(model));
                        base.set(latest);
                        stale.set(None);
                    }
                    Err(e) => error.set(Some(format!(
                        "Your edits do not apply to v{}: {e}. Discard them, or keep editing and save a copy by downloading.",
                        latest.version
                    ))),
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
                <strong>{ format!("Editing v{}", base.version) }</strong>
                <span class="muted">{ "Drag blocks, edit names and parameters in the inspector, Delete removes the selected block." }</span>
                <span class="spacer" />
                <span>{ format!("{count} change{}", if count == 1 { "" } else { "s" }) }</span>
                <button onclick={undo} disabled={count == 0 || *busy}>{ "Undo" }</button>
                <button onclick={discard} disabled={*busy}>{ "Discard" }</button>
                <input placeholder="Change message" value={(*message).clone()} oninput={set_message} disabled={*busy} />
                <button class="primary" onclick={save} disabled={count == 0 || *busy || stale.is_some()}>
                    { if *busy { "Working…" } else { "Save as new version" } }
                </button>
            </div>
        }
    };

    let model = if *editing {
        (*working).clone()
    } else {
        base.model.clone()
    };
    html! {
        <>
            { toolbar }
            if let Some(reason) = &*stale {
                <div class="edit-bar error">
                    { reason }
                    <button onclick={reapply} disabled={*busy}>{ "Reapply my edits on the latest version" }</button>
                </div>
            }
            if let Some(e) = &*error {
                <div class="edit-bar error">{ e }</div>
            }
            <DiagramView {model} fit_key={props.fit_key.clone()}
                on_edit={(*editing && !*busy).then(|| on_edit.clone())} />
        </>
    }
}
