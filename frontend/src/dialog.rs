//! The parameter dialog of a native catalog block: its parameters in their
//! sections, with typed controls, showing only the fields the block's
//! current settings use. Hidden fields keep their stored values.

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use unlinked_model::catalog::{self, ParameterDescriptor, ParameterKind};
use unlinked_model::edit::{Edit, SystemRef};
use unlinked_model::validation::{Diagnostic, DiagnosticTarget, Severity};
use unlinked_model::Block;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DialogProps {
    /// A native catalog block.
    pub block: Rc<Block>,
    pub system: SystemRef,
    /// Problems with the block, shown beside the parameters they name.
    pub problems: Rc<Vec<Diagnostic>>,
    /// Set while editing; without it the dialog is read-only.
    pub on_edit: Option<Callback<Edit>>,
}

#[function_component(ParameterDialog)]
pub fn parameter_dialog(props: &DialogProps) -> Html {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    let instance = use_state(|| NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let help = use_state(|| None::<&'static str>);
    let query = use_state(String::new);
    {
        let (help, query) = (help.clone(), query.clone());
        use_effect_with((props.block.id.clone(), props.system.clone()), move |_| {
            help.set(None);
            query.set(String::new());
        });
    }
    let b = &props.block;
    let set = |name: &'static str, current: Option<String>| {
        let (on_edit, system, id) = (props.on_edit.clone(), props.system.clone(), b.id.clone());
        move |value: String| {
            if let Some(on_edit) = &on_edit {
                if current.as_deref() != Some(value.as_str()) {
                    on_edit.emit(Edit::SetParameter {
                        system: system.clone(),
                        id: id.clone(),
                        name: name.into(),
                        value,
                    });
                }
            }
        }
    };
    let readonly = props.on_edit.is_none();
    let field = |p: &'static ParameterDescriptor| -> Html {
        let id = format!("parameter-{}-{}", *instance, p.name);
        let help_id = format!("{id}-help");
        let issues_id = format!("{id}-issues");
        let described_by = format!("{help_id} {issues_id}");
        let problems: Vec<_> = props.problems.iter().filter(|d| {
            matches!(&d.target, DiagnosticTarget::Block { parameter: Some(name), .. } if name == p.name)
        }).collect();
        let invalid = problems.iter().any(|d| d.severity == Severity::Error);
        let help_open = *help == Some(p.name);
        let toggle_help = {
            let help = help.clone();
            Callback::from(move |_: MouseEvent| {
                help.set(if help_open { None } else { Some(p.name) })
            })
        };
        let stored = b.param(p.name).map(str::to_string);
        // What applies when the block does not store the parameter.
        let shown = stored.clone().or(p.implicit_default.map(str::to_string));
        let set = set(p.name, stored.clone());
        // A choice among `values`; a current value outside them is shown as
        // kept rather than as one of them.
        let choice = |values: &[&'static str], set: Box<dyn Fn(String)>| {
            let current = shown.clone().unwrap_or_default();
            let known = values.contains(&current.as_str());
            let onchange = Callback::from(move |e: Event| {
                set(e.target_unchecked_into::<HtmlSelectElement>().value())
            });
            html! {
                <select id={id.clone()} aria-invalid={invalid.then_some("true")} aria-describedby={described_by.clone()} {onchange} disabled={readonly}>
                    if !known {
                        <option value={current.clone()} selected=true>
                            { format!("{} (kept as is)", if current.is_empty() { "Not set" } else { &current }) }
                        </option>
                    }
                    { for values.iter().map(|v| html! {
                        <option value={v.to_string()} selected={*v == current}>{ p.label_for(v) }</option>
                    }) }
                </select>
            }
        };
        let control = match p.kind {
            ParameterKind::Enum(values) => choice(values, Box::new(set)),
            // Only an on/off value is a checkbox.
            ParameterKind::Boolean if !matches!(shown.as_deref(), Some("on" | "off")) => {
                choice(&["on", "off"], Box::new(set))
            }
            ParameterKind::Boolean => {
                let checked = shown.as_deref() == Some("on");
                let onchange = Callback::from(move |e: Event| {
                    let on = e.target_unchecked_into::<HtmlInputElement>().checked();
                    set(if on { "on" } else { "off" }.to_string())
                });
                html! { <input id={id.clone()} aria-invalid={invalid.then_some("true")} aria-describedby={described_by.clone()} type="checkbox" {checked} {onchange} disabled={readonly} /> }
            }
            ParameterKind::Expression | ParameterKind::IntegerExpression | ParameterKind::Text => {
                let onchange = Callback::from(move |e: Event| {
                    set(e.target_unchecked_into::<HtmlInputElement>().value())
                });
                html! {
                    <input id={id.clone()} aria-invalid={invalid.then_some("true")} aria-describedby={described_by.clone()} class="param mono" value={stored.clone().unwrap_or_default()}
                        placeholder={p.implicit_default.unwrap_or("Not set")}
                        {onchange} readonly={readonly} />
                }
            }
        };
        html! {
            <div key={p.name} class="dialog-field">
                <div class="dialog-label-row">
                    <label for={id} class="dialog-label">
                        { p.label }
                        if let Some(units) = p.dialog.units {
                            <span class="muted">{ format!(" ({units})") }</span>
                        }
                    </label>
                    if !p.dialog.help.is_empty() {
                        <button type="button" class="parameter-help-button" aria-label={format!("Help for {}",p.label)}
                            aria-expanded={help_open.to_string()} aria-controls={help_id.clone()} onclick={toggle_help}>{ "?" }</button>
                    }
                </div>
                { control }
                <p id={help_id} class="parameter-help-text" hidden={!help_open}>{p.dialog.help}</p>
                <div id={issues_id} aria-live="polite">
                    { for problems.iter().map(|d| html! {
                        <div class={match d.severity { Severity::Error => "problem error", Severity::Warning => "problem warning" }}>{ &d.message }</div>
                    }) }
                </div>
            </div>
        }
    };
    let Some(descriptor) = catalog::find(&b.block_type) else {
        return html! {};
    };
    let sections = descriptor.dialog_sections(&b.parameters);
    let visible_count = sections
        .iter()
        .flat_map(|s| &s.fields)
        .filter(|(_, visible)| *visible)
        .count();
    let needle = query.trim().to_lowercase();
    let matches = |p: &ParameterDescriptor, visible: bool| {
        visible
            && (needle.is_empty()
                || p.name.to_lowercase().contains(&needle)
                || p.label.to_lowercase().contains(&needle))
    };
    let matched = sections
        .iter()
        .flat_map(|s| &s.fields)
        .filter(|(p, visible)| matches(p, *visible))
        .count();
    let search = {
        let query = query.clone();
        Callback::from(move |e: InputEvent| {
            query.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    html! {
        <div class="parameter-dialog">
            if visible_count > 6 || !query.is_empty() {
                <input type="search" class="parameter-search" aria-label="Search parameters" placeholder="Find a parameter…" value={(*query).clone()} oninput={search} />
            }
            if matched == 0 { <p class="muted" role="status">{ "No matching parameters." }</p> }
            { for sections.iter().filter(|s| s.fields.iter().any(|(p, visible)| matches(p, *visible))).map(|s| html! {
                <fieldset key={s.name}>
                    <legend>{ s.name }</legend>
                    { for s.fields.iter().filter(|(p, visible)| matches(p, *visible)).map(|(p, _)| field(p)) }
                </fieldset>
            }) }
        </div>
    }
}
