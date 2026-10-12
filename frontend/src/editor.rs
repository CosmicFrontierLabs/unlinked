//! Edit a stored model and save the result as a new file version.
//!
//! Edits are recorded as a batch against a pinned base version. The batch
//! is applied to a copy of the base IR for immediate feedback, and saving
//! replays it onto the base file bytes with `unlinked_import::patch`, so
//! everything the IR does not model survives. A save is refused when the
//! file has a newer version than the base; the user can then explicitly
//! reapply the batch on top of the latest version.

use crate::api;
use crate::diagnostics_worker::{DiagnosticOutcome, DiagnosticsWorker};
use crate::diagram::DiagramView;
use crate::icons;
use crate::settings::ModelSettings;
use crate::sim::initial_options;
use gloo_events::{EventListener, EventListenerOptions};
use std::rc::Rc;
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::Model;
use unlinked_sim::diagnose::{CheckMode, DiagnosticContext, DiagnosticReport, SimulationCheck};
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
    /// Whether the editor is shown. A hidden editor keeps its edits but
    /// takes no keyboard shortcuts.
    #[prop_or(true)]
    pub active: bool,
    /// Told whether there are unsaved edits, whenever that changes.
    #[prop_or_default]
    pub on_dirty_change: Callback<bool>,
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

/// The compile check of the shown model snapshot.
enum Compile {
    Idle,
    Running,
    Done(Rc<DiagnosticReport>),
    Failed(String),
}

/// What a user action did, as undo and redo name it: by its most telling
/// edit (a paste is adding blocks, though it also connects them).
fn describe(group: &[Edit]) -> String {
    let count = |f: fn(&Edit) -> bool| group.iter().filter(|e| f(e)).count();
    let blocks = |n: usize| format!("{n} block{}", if n == 1 { "" } else { "s" });
    let first = |f: fn(&Edit) -> bool| group.iter().find(|e| f(e));
    if let Some(e) = first(|e| {
        matches!(
            e,
            Edit::CreateSubsystem { .. } | Edit::ExpandSubsystem { .. }
        )
    }) {
        return match e {
            Edit::CreateSubsystem { .. } => "Group into a subsystem".into(),
            _ => "Expand a subsystem".into(),
        };
    }
    let added = count(|e| matches!(e, Edit::AddBlock { .. }));
    if added > 0 {
        return format!("Add {}", blocks(added));
    }
    let deleted = count(|e| matches!(e, Edit::DeleteBlock { .. }));
    if deleted > 0 {
        return format!("Delete {}", blocks(deleted));
    }
    match group.first() {
        Some(Edit::MoveBlock { .. }) => {
            format!(
                "Move {}",
                blocks(count(|e| matches!(e, Edit::MoveBlock { .. })))
            )
        }
        Some(Edit::SetOrientation { .. }) => "Rotate or flip".into(),
        Some(Edit::SetParameter { name, .. }) => format!("Set {name}"),
        Some(Edit::RenameBlock { name, .. }) => format!("Rename to {}", name.replace('\n', " ")),
        Some(Edit::Connect { .. }) => "Connect".into(),
        Some(Edit::Disconnect { .. }) => "Disconnect".into(),
        Some(Edit::SetRoute { .. } | Edit::SetTrunkRoute { .. }) => "Reroute a line".into(),
        Some(Edit::SetSignalName { .. }) => "Name a signal".into(),
        Some(Edit::AddAnnotation { .. }) => "Add an annotation".into(),
        Some(Edit::MoveAnnotation { .. }) => "Move an annotation".into(),
        Some(Edit::SetAnnotationText { .. }) => "Edit an annotation".into(),
        Some(Edit::DeleteAnnotation { .. }) => "Delete an annotation".into(),
        Some(Edit::SetConfig { key, .. }) => format!("Set {key}"),
        _ => "Edit".into(),
    }
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
    // Editors edit in place, as in Simulink: there is no separate edit mode.
    let editing = props.can_edit;
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
    let worker = use_mut_ref(DiagnosticsWorker::default);
    let generation = use_mut_ref(|| 0u64);
    // The compile check, with the model snapshot it is for.
    let compile = use_state(|| (0usize, Compile::Idle));
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
            // Grouping, expanding and solver settings depend on raw file
            // records the IR does not model, so the file may refuse what
            // the preview accepts: try saving first, so a refusal shows now
            // rather than at Save.
            let raw = group.iter().any(|e| {
                matches!(
                    e,
                    Edit::CreateSubsystem { .. }
                        | Edit::ExpandSubsystem { .. }
                        | Edit::SetConfig { .. }
                )
            });
            if raw {
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

    let discard = {
        let (pending, redo_stack, working, error, stale, base) = (
            pending.clone(),
            redo_stack.clone(),
            working.clone(),
            error.clone(),
            stale.clone(),
            base.clone(),
        );
        Callback::from(move |_: MouseEvent| {
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
    let save = {
        let (mounted, pending, message, busy, error, stale, base) = (
            mounted.clone(),
            pending.clone(),
            message.clone(),
            busy.clone(),
            error.clone(),
            stale.clone(),
            base.clone(),
        );
        let (project_id, file_id, on_saved) =
            (props.project_id, props.file_id, props.on_saved.clone());
        let redo_stack = redo_stack.clone();
        Callback::from(move |()| {
            if *busy || stale.is_some() {
                return;
            }
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
            // Unless described, a version says what was done: its first
            // few actions.
            let msg = if message.trim().is_empty() {
                let mut said: Vec<String> = pending.iter().take(3).map(|g| describe(g)).collect();
                if pending.len() > 3 {
                    said.push(format!("and {} more", pending.len() - 3));
                }
                said.join("; ")
            } else {
                message.trim().to_string()
            };
            busy.set(true);
            let (mounted, busy, error, stale, pending, on_saved, message) = (
                mounted.clone(),
                busy.clone(),
                error.clone(),
                stale.clone(),
                pending.clone(),
                on_saved.clone(),
                message.clone(),
            );
            let base = (*base).clone();
            let redo_stack = redo_stack.clone();
            spawn_local(async move {
                let result = async {
                    // Metadata refreshes preserve this editor, so the file
                    // may have been renamed since the base was loaded.
                    let current = api::file(project_id, file_id).await?;
                    let extension = |path: &str| path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                    if extension(&current.path) != extension(&base.path) {
                        return Err(api::ApiError { status: 400, message: "The file format changed after this model was opened. Restore its original extension before saving these edits.".into() });
                    }
                    api::upload(project_id, &current.path, &msg, &patched, Some(base.version_id)).await
                }.await;
                if !*mounted.borrow() {
                    return;
                }
                busy.set(false);
                match result {
                    // The edited model stays shown until the saved version
                    // replaces this editor.
                    Ok(_) => {
                        pending.set(Vec::new());
                        redo_stack.set(Vec::new());
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
    {
        let on_dirty_change = props.on_dirty_change.clone();
        use_effect_with(count > 0, move |dirty| on_dirty_change.emit(*dirty));
    }
    // A save asked for from the keyboard, made once the render after the
    // request has the edits it should include.
    let save_requested = use_state(|| false);
    {
        let (save, save_requested) = (save.clone(), save_requested.clone());
        use_effect_with(*save_requested, move |requested| {
            if *requested {
                save_requested.set(false);
                save.emit(());
            }
        });
    }
    // Ctrl/Cmd+Z undoes, Ctrl/Cmd+Shift+Z or Ctrl+Y redoes and Ctrl/Cmd+S
    // saves, unless typing in a field. Re-registered each render so it acts
    // on the current state.
    {
        let (undo, redo, save_requested) = (undo.clone(), redo.clone(), save_requested.clone());
        let active = props.active && editing && !*busy;
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
                        let field = e
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                            .filter(|t| {
                                matches!(t.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                            });
                        if !(e.ctrl_key() || e.meta_key()) {
                            return;
                        }
                        match (e.key().to_ascii_lowercase().as_str(), e.shift_key()) {
                            // Saving from a field too. Parameter and setting
                            // fields commit their value when they lose focus,
                            // so leave the field first and save after the
                            // edit it makes.
                            ("s", false) => {
                                if let Some(field) = &field {
                                    let _ = field.blur();
                                }
                                save_requested.set(true);
                            }
                            _ if field.is_some() => return,
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
    let settings_button = {
        let settings = settings.clone();
        html! {
            <button class={classes!("tool", (*settings).then_some("active"))}
                title="Solver, start and stop time, step sizes and tolerances"
                onclick={Callback::from(move |_: MouseEvent| settings.set(!*settings))}>
                { icons::settings() }<span>{ "Settings" }</span>
            </button>
        }
    };
    // Shown at the start of the diagram's toolstrip.
    let leading = editing.then(|| {
        html! {
            <span class="tool-group">
                <button class="tool icon" onclick={undo.reform(|_: MouseEvent| ())}
                    disabled={count == 0 || *busy}
                    title={pending.last().map_or("Undo (Ctrl+Z)".into(), |g| format!("Undo: {} (Ctrl+Z)", describe(g)))}>
                    { icons::undo() }
                </button>
                <button class="tool icon" onclick={redo.reform(|_: MouseEvent| ())}
                    disabled={redo_stack.is_empty() || *busy}
                    title={redo_stack.last().map_or("Redo (Ctrl+Shift+Z)".into(), |g| format!("Redo: {} (Ctrl+Shift+Z)", describe(g)))}>
                    { icons::redo() }
                </button>
            </span>
        }
    });
    // Shown at the end: settings, then the save controls once there is
    // something to save.
    let trailing = html! {
        <>
            { settings_button }
            if editing && count > 0 {
                <span class="tool-group save-group">
                    <span class="changes" title={format!("Unsaved changes to v{}", base.version)}>
                        { format!("{count} unsaved change{}", if count == 1 { "" } else { "s" }) }
                    </span>
                    <button class="tool" onclick={discard} disabled={*busy}
                        title="Drop every unsaved change">{ "Revert" }</button>
                    <input class="change-message" placeholder="Describe the change"
                        value={(*message).clone()} oninput={set_message} disabled={*busy} />
                    <button class="tool primary" onclick={save.reform(|_: MouseEvent| ())}
                        disabled={*busy || stale.is_some()}
                        title={format!("Save as v{} (Ctrl+S)", base.version + 1)}>
                        { if *busy { "Saving…".to_string() } else { format!("Save v{}", base.version + 1) } }
                    </button>
                </span>
            }
        </>
    };

    let model = if editing {
        (*working).clone()
    } else {
        base.model.clone()
    };
    // Static checks: cheap enough to rerun after every edit.
    let snapshot = Rc::as_ptr(&model) as usize;
    let report = {
        let model = model.clone();
        use_memo(snapshot, move |_| {
            unlinked_sim::diagnose::diagnose(&model, &Default::default())
        })
    };
    // A compile check runs on request in a worker, for one model snapshot:
    // its state is kept with that snapshot and ignored for any other, and
    // an edit cancels a check still running and forgets it, so a later
    // snapshot reusing the address cannot pick it up.
    {
        let (worker, compile) = (worker.clone(), compile.clone());
        use_effect_with(snapshot, move |_| {
            worker.borrow_mut().cancel();
            compile.set((0, Compile::Idle));
        });
    }
    // The run settings a check uses: the Simulate tab's initial ones.
    let (run_options, substitutions) = initial_options(&model.config);
    let check_compile = {
        let (worker, generation, compile, model) = (
            worker.clone(),
            generation.clone(),
            compile.clone(),
            model.clone(),
        );
        let options = run_options.clone();
        Callback::from(move |()| {
            let current = {
                let mut g = generation.borrow_mut();
                *g += 1;
                *g
            };
            let context = DiagnosticContext {
                mode: CheckMode::Compile,
                options: Some(options.clone()),
                ..Default::default()
            };
            let done = {
                let (compile, generation) = (compile.clone(), generation.clone());
                Callback::from(move |outcome: DiagnosticOutcome| {
                    if outcome.generation == *generation.borrow() {
                        compile.set((
                            snapshot,
                            match outcome.result {
                                Ok(report) => Compile::Done(Rc::new(report)),
                                Err(e) => Compile::Failed(e),
                            },
                        ));
                    }
                })
            };
            let state = match worker.borrow_mut().start(current, &model, &context, done) {
                Ok(()) => Compile::Running,
                Err(e) => Compile::Failed(e),
            };
            compile.set((snapshot, state));
        })
    };
    let current_compile = match &*compile {
        (at, state) if *at == snapshot => state,
        _ => &Compile::Idle,
    };
    let compiled = match current_compile {
        Compile::Done(report) => Some(report.clone()),
        _ => None,
    };
    let shown = compiled.as_deref().unwrap_or(&report);
    let problems = Rc::new(shown.diagnostics.clone());
    let problems_truncated = shown.truncated || shown.warnings_omitted;
    let settings_used = {
        let o = &run_options;
        let mut text = format!(
            "Run settings: {}, {} to {} s, output step {}",
            crate::sim::solver_name(o.solver),
            o.start,
            o.stop,
            o.step
        );
        if !substitutions.is_empty() {
            text.push_str(&format!(" ({})", substitutions.join(" ")));
        }
        text
    };
    let compile_status: AttrValue = match current_compile {
        Compile::Idle => "".into(),
        Compile::Running => format!("Checking whether the simulator compiles the model… {settings_used}.").into(),
        Compile::Failed(e) => format!("Compile check failed: {e}").into(),
        Compile::Done(report) => match report.simulation {
            SimulationCheck::Compiled => format!("The simulator compiles the model. Nothing was simulated, so this says nothing about results. {settings_used}.").into(),
            SimulationCheck::Rejected => format!("The simulator does not compile the model; see the problems. {settings_used}.").into(),
            SimulationCheck::Incomplete | SimulationCheck::NotChecked => "The compile check could not finish; see the problems.".into(),
        },
    };
    let compile_running = matches!(current_compile, Compile::Running);
    html! {
        <>
            if let Some(reason) = &*stale {
                <div class="edit-bar error">
                    { reason }
                    <button onclick={reapply} disabled={*busy}>{ "Reapply my edits on the latest version" }</button>
                </div>
            }
            if let Some(e) = &*error {
                <div class="edit-bar error">
                    <span>{ e }</span>
                    <span class="spacer" />
                    <button class="tool icon" title="Dismiss" onclick={{
                        let error = error.clone();
                        Callback::from(move |_: MouseEvent| error.set(None))
                    }}>{ icons::close() }</button>
                </div>
            }
            if *settings {
                <ModelSettings config={model.config.clone()}
                    on_edit={(editing && !*busy).then(|| on_edit.clone())} />
            }
            <DiagramView {model} fit_key={props.fit_key.clone()} active={props.active} {problems} {problems_truncated}
                leading={leading.unwrap_or_default()} {trailing}
                on_compile={(!compile_running).then_some(check_compile)}
                {compile_status}
                on_settings={Callback::from({
                    let settings = settings.clone();
                    move |()| settings.set(true)
                })}
                on_edit={(editing && !*busy).then(|| on_edit.clone())}
                on_error={Callback::from({
                    let error = error.clone();
                    move |message: String| error.set(Some(message))
                })} />
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlinked_model::{BlockId, Rect};

    fn moved(id: &str) -> Edit {
        Edit::MoveBlock {
            system: vec![],
            id: BlockId::from(id),
            position: Rect::new(0.0, 0.0, 10.0, 10.0),
        }
    }

    #[test]
    fn actions_are_named_by_their_most_telling_edit() {
        assert_eq!(describe(&[moved("1")]), "Move 1 block");
        assert_eq!(describe(&[moved("1"), moved("2")]), "Move 2 blocks");
        let paste = [
            Edit::AddBlock {
                system: vec![],
                id: BlockId::from("9"),
                block_type: "Gain".into(),
                name: "g1".into(),
                position: Rect::new(0.0, 0.0, 10.0, 10.0),
            },
            moved("1"),
        ];
        assert_eq!(describe(&paste), "Add 1 block");
        let set = Edit::SetConfig {
            key: "StopTime".into(),
            value: "5".into(),
        };
        assert_eq!(describe(&[set]), "Set StopTime");
        assert_eq!(describe(&[]), "Edit");
    }
}
