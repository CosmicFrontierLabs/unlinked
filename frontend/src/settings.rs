//! The model's stored solver settings, checked against what Simulink allows
//! and what the simulator supports. Values are written back verbatim: an
//! unrecognized solver or an expression is kept as it is, never replaced.

use unlinked_model::config::{solver_descriptor, validate_config, SolverKind, SOLVERS};
use unlinked_model::edit::Edit;
use unlinked_model::validation::{Diagnostic, DiagnosticTarget, Severity};
use unlinked_model::SimConfig;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct SettingsProps {
    pub config: SimConfig,
    /// Set while editing; without it the settings are read-only.
    #[prop_or_default]
    pub on_edit: Option<Callback<Vec<Edit>>>,
}

/// Editable settings: key, label, and which solvers use it (`None`: all).
const FIELDS: [(&str, &str, Option<SolverKind>); 8] = [
    ("StartTime", "Start time", None),
    ("StopTime", "Stop time", None),
    ("FixedStep", "Fixed step size", Some(SolverKind::FixedStep)),
    (
        "RelTol",
        "Relative tolerance",
        Some(SolverKind::VariableStep),
    ),
    (
        "AbsTol",
        "Absolute tolerance",
        Some(SolverKind::VariableStep),
    ),
    ("MaxStep", "Max step size", Some(SolverKind::VariableStep)),
    ("MinStep", "Min step size", Some(SolverKind::VariableStep)),
    (
        "InitialStep",
        "Initial step size",
        Some(SolverKind::VariableStep),
    ),
];

fn messages(diagnostics: &[&Diagnostic]) -> Html {
    diagnostics
        .iter()
        .map(|d| {
            let class = match d.severity {
                Severity::Error => "problem error",
                Severity::Warning => "problem warning",
            };
            html! { <div {class}>{ &d.message }</div> }
        })
        .collect()
}

#[function_component(ModelSettings)]
pub fn model_settings(props: &SettingsProps) -> Html {
    let config = &props.config;
    let report = validate_config(config);
    let about = |key: &str| -> Vec<&Diagnostic> {
        report
            .diagnostics
            .iter()
            .filter(
                |d| matches!(&d.target, DiagnosticTarget::Config { parameter } if parameter == key),
            )
            .collect()
    };
    let set = {
        let (on_edit, config) = (props.on_edit.clone(), config.clone());
        move |key: &'static str, value: String| {
            if let Some(on_edit) = &on_edit {
                let current = match key {
                    "Solver" => config.solver.as_deref(),
                    _ => config.raw.get(key).map(String::as_str),
                };
                let value = value.trim().to_string();
                if !value.is_empty() && current != Some(value.as_str()) {
                    on_edit.emit(vec![Edit::SetConfig {
                        key: key.into(),
                        value,
                    }]);
                }
            }
        }
    };
    let readonly = props.on_edit.is_none();

    let stored = config.solver.as_deref();
    let descriptor = stored.and_then(solver_descriptor);
    let kind = descriptor.map(|d| d.kind);
    let option = |value: &str, label: String| {
        let selected = descriptor.map(|d| d.value) == Some(value);
        html! { <option value={value.to_string()} {selected}>{ label }</option> }
    };
    let group = |kind: SolverKind, label: &str| {
        html! {
            <optgroup label={label.to_string()}>
                { for SOLVERS.iter().filter(|s| s.kind == kind).map(|s| {
                    let note = if s.simulation_solver.is_some() { "" } else { " (not simulated)" };
                    option(s.value, format!("{}: {}{note}", s.value, s.label))
                }) }
            </optgroup>
        }
    };
    let on_solver = {
        let set = set.clone();
        Callback::from(move |e: Event| {
            set(
                "Solver",
                e.target_unchecked_into::<HtmlSelectElement>().value(),
            )
        })
    };
    let solver = html! {
        <label class="setting">
            <span>{ "Solver" }</span>
            <select onchange={on_solver} disabled={readonly}>
                if stored.is_none() {
                    <option value="" selected=true disabled=true>{ "Not set" }</option>
                }
                if let (Some(s), None) = (stored, descriptor) {
                    <option value={s.to_string()} selected=true>{ format!("{s} (unrecognized, kept as is)") }</option>
                }
                { group(SolverKind::FixedStep, "Fixed step") }
                { group(SolverKind::VariableStep, "Variable step") }
            </select>
            { messages(&about("Solver")) }
        </label>
    };

    let fields = FIELDS
        .iter()
        .filter(|(key, _, used_by)| {
            used_by.is_none() || kind.is_none() || *used_by == kind || config.raw.contains_key(*key)
        })
        .map(|&(key, label, used_by)| {
            let value = config.raw.get(key).cloned().unwrap_or_default();
            let unused = used_by.is_some() && kind.is_some() && used_by != kind;
            let onchange = {
                let set = set.clone();
                Callback::from(move |e: Event| {
                    set(key, e.target_unchecked_into::<HtmlInputElement>().value())
                })
            };
            html! {
                <label class="setting">
                    <span>{ label }</span>
                    <input class="mono" {value} placeholder="Not set" {onchange} disabled={readonly}
                        title={unused.then_some("Not used by the selected solver")} />
                    { messages(&about(key)) }
                </label>
            }
        })
        .collect::<Html>();
    // Diagnostics about settings without a field, such as SolverType.
    let shown = |p: &str| p == "Solver" || FIELDS.iter().any(|(key, ..)| *key == p);
    let others: Vec<&Diagnostic> = report
        .diagnostics
        .iter()
        .filter(
            |d| matches!(&d.target, DiagnosticTarget::Config { parameter } if !shown(parameter)),
        )
        .collect();

    let summary = if !report.is_valid() {
        "These settings are invalid as stored."
    } else if report.simulation_supported {
        "The simulator supports these settings. This says nothing about the model's blocks."
    } else {
        "The simulator cannot use these settings as stored; choose explicit run settings when simulating."
    };
    html! {
        <div class="settings-panel">
            <div class="settings-grid">
                { solver }
                { fields }
            </div>
            { messages(&others) }
            <p class="muted">
                { summary }
                if readonly { { " Read-only: start editing to change them." } }
            </p>
        </div>
    }
}
