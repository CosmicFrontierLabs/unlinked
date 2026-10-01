//! The parameter dialog of a native catalog block: its parameters in their
//! sections, with typed controls, showing only the fields the block's
//! current settings use. Hidden fields keep their stored values.

use std::rc::Rc;
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
                <select {onchange} disabled={readonly}>
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
                html! { <input type="checkbox" {checked} {onchange} disabled={readonly} /> }
            }
            ParameterKind::Expression | ParameterKind::IntegerExpression | ParameterKind::Text => {
                let onchange = Callback::from(move |e: Event| {
                    set(e.target_unchecked_into::<HtmlInputElement>().value())
                });
                html! {
                    <input class="param mono" value={stored.clone().unwrap_or_default()}
                        placeholder={p.implicit_default.unwrap_or("Not set")}
                        {onchange} disabled={readonly} />
                }
            }
        };
        let problems = props.problems.iter().filter(|d| {
            matches!(&d.target, DiagnosticTarget::Block { parameter: Some(name), .. } if name == p.name)
        });
        html! {
            <label class="dialog-field" title={p.dialog.help}>
                <span class="dialog-label">
                    { p.label }
                    if let Some(units) = p.dialog.units {
                        <span class="muted">{ format!(" ({units})") }</span>
                    }
                </span>
                { control }
                { for problems.map(|d| html! {
                    <div class={match d.severity { Severity::Error => "problem error", Severity::Warning => "problem warning" }}>
                        { &d.message }
                    </div>
                }) }
            </label>
        }
    };
    let Some(descriptor) = catalog::find(&b.block_type) else {
        return html! {};
    };
    let sections = descriptor.dialog_sections(&b.parameters);
    html! {
        <div class="parameter-dialog">
            { for sections.iter().filter(|s| s.fields.iter().any(|(_, visible)| *visible)).map(|s| html! {
                <fieldset>
                    <legend>{ s.name }</legend>
                    { for s.fields.iter().filter(|(_, visible)| *visible).map(|(p, _)| field(p)) }
                </fieldset>
            }) }
        </div>
    }
}
