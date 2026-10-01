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
use crate::settings::ModelSettings;
use gloo_events::{EventListener, EventListenerOptions};
use std::rc::Rc;
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::Model;
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, InputEvent, KeyboardEvent};
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
    // Edits by user action, so undo and redo move whole actions.
    let pending = use_state(Vec::<Vec<Edit>>::new);
    let redo_stack = use_state(Vec::<Vec<Edit>>::new);
    let working = use_state(|| props.base.model.clone());
    let error = use_state(|| None::<String>);
    // Why the last save was refused because the file changed meanwhile.
    // Kept apart from `error`, which later edits clear, so the recovery
    // action stays visible until the user reapplies or discards.
    let stale = use_state(|| None::<String>);
    let message = use_state(String::new);
    let busy = use_state(|| false);
    let settings = use_state(|| false);
    // Cleared on unmount so a request finishing afterwards neither updates
    // state nor navigates.
    let mounted = use_mut_ref(|| true);
    {
        let mounted = mounted.clone();
        use_effect_with((), move |_| move || *mounted.borrow_mut() = false);
    }

    let on_edit = {
        let (pending, redo_stack, working, error, base) = (
            pending.clone(),
            redo_stack.clone(),
            working.clone(),
            error.clone(),
            base.clone(),
        );
        Callback::from(move |group: Vec<Edit>| {
            // Grouping and expanding move raw file records the IR does not
            // model, so the file may refuse what the preview accepts: try
            // saving first, so a refusal shows now rather than at Save.
            let hierarchy = group.iter().any(|e| {
                matches!(
                    e,
                    Edit::CreateSubsystem { .. } | Edit::ExpandSubsystem { .. }
                )
            });
            if hierarchy {
                let mut all = pending.concat();
                all.extend(group.iter().cloned());
                if let Err(e) = unlinked_import::patch::apply_edits(&base.path, &base.bytes, &all) {
                    error.set(Some(e.to_string()));
                    return;
                }
            }
            let mut next = (**working).clone();
            match apply_batch(&mut next, &group) {
                Ok(()) => {
                    let mut p = (*pending).clone();
                    p.push(group);
                    pending.set(p);
                    redo_stack.set(Vec::new());
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
        let (editing, pending, redo_stack, working, error, stale, base) = (
            editing.clone(),
            pending.clone(),
            redo_stack.clone(),
            working.clone(),
            error.clone(),
            stale.clone(),
            base.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            editing.set(false);
            pending.set(Vec::new());
            redo_stack.set(Vec::new());
            working.set(base.model.clone());
            error.set(None);
            stale.set(None);
        })
    };
    let undo = {
        let (pending, redo_stack, working, error, base) = (
            pending.clone(),
            redo_stack.clone(),
            working.clone(),
            error.clone(),
            base.clone(),
        );
        Callback::from(move |_: ()| {
            let mut p = (*pending).clone();
            let Some(group) = p.pop() else {
                return;
            };
            match replay(&base.model, &p.concat()) {
                Ok(m) => {
                    working.set(Rc::new(m));
                    pending.set(p);
                    let mut r = (*redo_stack).clone();
                    r.push(group);
                    redo_stack.set(r);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
        })
    };
    let redo = {
        let (pending, redo_stack, working, error) = (
            pending.clone(),
            redo_stack.clone(),
            working.clone(),
            error.clone(),
        );
        Callback::from(move |_: ()| {
            let mut r = (*redo_stack).clone();
            let Some(group) = r.pop() else {
                return;
            };
            let mut next = (**working).clone();
            match apply_batch(&mut next, &group) {
                Ok(()) => {
                    let mut p = (*pending).clone();
                    p.push(group);
                    pending.set(p);
                    redo_stack.set(r);
                    working.set(Rc::new(next));
                    error.set(None);
                }
                Err(e) => error.set(Some(e.to_string())),
            }
        })
    };
    // Ctrl/Cmd+Z undoes, Ctrl/Cmd+Shift+Z or Ctrl+Y redoes, unless typing in
    // a field. Re-registered each render so it acts on the current state.
    {
        let (undo, redo, active) = (undo.clone(), redo.clone(), *editing && !*busy);
        use_effect(move || {
            // Not passive, so the browser's own shortcut can be suppressed.
            let listener = active.then(|| {
                EventListener::new_with_options(
                    &gloo_utils::document(),
                    "keydown",
                    EventListenerOptions::enable_prevent_default(),
                    move |e| {
                        let Some(e) = e.dyn_ref::<KeyboardEvent>() else {
                            return;
                        };
                        let typing = e
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                            .is_some_and(|t| {
                                matches!(t.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                            });
                        if typing || !(e.ctrl_key() || e.meta_key()) {
                            return;
                        }
                        match (e.key().to_ascii_lowercase().as_str(), e.shift_key()) {
                            ("z", false) => undo.emit(()),
                            ("z", true) | ("y", _) => redo.emit(()),
                            _ => return,
                        }
                        e.prevent_default();
                    },
                )
            });
            move || drop(listener)
        });
    }
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
        let redo_stack = redo_stack.clone();
        Callback::from(move |_: MouseEvent| {
            let edits = pending.concat();
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
                    pending.len(),
                    if pending.len() == 1 { "" } else { "s" }
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
            let redo_stack = redo_stack.clone();
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
                        redo_stack.set(Vec::new());
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
                // carry over if every one still applies to the new version,
                // in its file as well as its IR (the file can refuse what
                // the IR accepts, e.g. when grouping or expanding).
                let edits = pending.concat();
                let replayed = replay(&latest.model, &edits).and_then(|model| {
                    unlinked_import::patch::apply_edits(&latest.path, &latest.bytes, &edits)
                        .map(|_| model)
                        .map_err(|e| e.to_string())
                });
                match replayed {
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
    let settings_button = {
        let settings = settings.clone();
        html! {
            <button class={classes!((*settings).then_some("active"))}
                onclick={Callback::from(move |_: MouseEvent| settings.set(!*settings))}>
                { "Model settings" }
            </button>
        }
    };
    let toolbar = if !*editing {
        html! {
            <div class="edit-bar">
                { settings_button }
                if props.can_edit {
                    <button onclick={start}>{ "Edit" }</button>
                }
            </div>
        }
    } else {
        html! {
            <div class="edit-bar editing">
                <strong>{ format!("Editing v{}", base.version) }</strong>
                <span class="muted">{ "Drag on empty space or Shift-click to select; Ctrl+C/Ctrl+V copies, Ctrl+G groups into a subsystem (Ctrl+Shift+G expands), Ctrl+R rotates, Ctrl+I flips, Delete removes; middle-drag pans." }</span>
                <span class="spacer" />
                { settings_button }
                <span>{ format!("{count} change{}", if count == 1 { "" } else { "s" }) }</span>
                <button onclick={undo.reform(|_: MouseEvent| ())} disabled={count == 0 || *busy} title="Ctrl+Z">{ "Undo" }</button>
                <button onclick={redo.reform(|_: MouseEvent| ())} disabled={redo_stack.is_empty() || *busy} title="Ctrl+Shift+Z">{ "Redo" }</button>
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
            if *settings {
                <ModelSettings config={model.config.clone()}
                    on_edit={(*editing && !*busy).then(|| on_edit.clone())} />
            }
            <DiagramView {model} fit_key={props.fit_key.clone()}
                on_edit={(*editing && !*busy).then(|| on_edit.clone())}
                on_error={Callback::from({
                    let error = error.clone();
                    move |message: String| error.set(Some(message))
                })} />
        </>
    }
}
