//! Recorded setup is separate from the editable form and only copied on request.
use super::parse_workspace;
use shared::{SimulationRequest, Solver};
use std::collections::BTreeMap;
use yew::prelude::*;

pub(super) fn workspace_text(values: &BTreeMap<String, String>) -> Result<String, String> {
    let text = values
        .iter()
        .map(|(name, value)| format!("{name} = {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    // API-created runs may contain multiline expressions this line-based form
    // cannot represent. Never silently change their meaning on reuse.
    if parse_workspace(&text).as_ref().ok() != Some(values) {
        return Err("This workspace contains expressions the run form cannot represent exactly. Copy them from Recorded setup and review them manually.".into());
    }
    Ok(text)
}

pub(super) fn reuse_inputs(
    current: &BTreeMap<String, String>,
    recorded: &BTreeMap<String, String>,
    ports: &[(String, String)],
) -> BTreeMap<String, String> {
    ports
        .iter()
        .filter_map(|(id, _)| {
            recorded
                .get(id)
                .or_else(|| current.get(id))
                .map(|value| (id.clone(), value.clone()))
        })
        .collect()
}

pub(super) fn view(
    request: &SimulationRequest,
    version: Option<i32>,
    ports: &[(String, String)],
    script_name: Option<&str>,
    disabled: bool,
    reuse: Callback<MouseEvent>,
) -> Html {
    let o = &request.options;
    let solver = match o.solver {
        Solver::Euler => "Euler",
        Solver::Rk4 => "RK4",
        Solver::Rk45 => "RK45",
    };
    html! {
        <details class="recorded-setup">
            <summary>{"Recorded setup"}</summary>
            <p>{format!("Settings used for this run of v{}. Viewing results does not change the run form.", version.map(|v| v.to_string()).unwrap_or_else(|| "?".into()))}</p>
            <dl class="setup-options">
                <dt>{"Solver"}</dt><dd>{solver}</dd>
                <dt>{"Time range (s)"}</dt><dd>{format!("{:?} to {:?}", o.start, o.stop)}</dd>
                <dt>{"Output step (s)"}</dt><dd>{format!("{:?}", o.step)}</dd>
                if o.solver == Solver::Rk45 {
                    <dt>{"Relative tolerance"}</dt><dd>{format!("{:?}", o.relative_tolerance)}</dd>
                    <dt>{"Absolute tolerance"}</dt><dd>{format!("{:?}", o.absolute_tolerance)}</dd>
                }
                <dt>{"Sample limit"}</dt><dd>{o.max_samples}</dd>
                if o.solver == Solver::Rk45 {<dt>{"Internal-step limit"}</dt><dd>{o.max_internal_steps}</dd>}
                <dt>{"Initialization script"}</dt><dd>{request.init_script.as_ref().map(|s| format!("{} · {}", script_name.map(str::to_owned).unwrap_or_else(|| s.file_id.to_string()), s.version.map(|v| format!("v{v}")).unwrap_or_else(|| "version not recorded".into()))).unwrap_or_else(|| "None".into())}</dd>
            </dl>
            if !request.workspace.is_empty() {
                <h5>{"Workspace expressions"}</h5>
                <pre>{request.workspace.iter().map(|(k,v)| format!("{k} = {v}")).collect::<Vec<_>>().join("\n")}</pre>
            }
            if !request.inputs.is_empty() {
                <h5>{"Constant root inputs"}</h5>
                <dl class="setup-options">{for request.inputs.iter().map(|(id, value)| html!{
                    <><dt>{ports.iter().find(|(port,_)| port == id).map(|(_,name)| format!("{name} ({id})")).unwrap_or_else(|| format!("Block {id}"))}</dt><dd><code>{value}</code></dd></>
                })}</dl>
            }
            <button onclick={reuse} {disabled}>{"Use recorded setup"}</button>
            <p class="muted">{"Replaces the form’s solver settings, workspace, matching root inputs and initialization script. Review before running; execution limits use this app’s defaults."}</p>
        </details>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reuse_keeps_new_inputs_and_replaces_only_matching_ports() {
        let current = BTreeMap::from([("1".into(), "old".into()), ("2".into(), "keep".into())]);
        let recorded = BTreeMap::from([
            ("1".into(), "recorded".into()),
            ("gone".into(), "skip".into()),
        ]);
        let ports = vec![("1".into(), "a".into()), ("2".into(), "b".into())];
        assert_eq!(
            reuse_inputs(&current, &recorded, &ports),
            BTreeMap::from([("1".into(), "recorded".into()), ("2".into(), "keep".into())])
        );
    }
    #[test]
    fn reuse_preserves_expressions_or_refuses_before_changing_the_form() {
        let values = BTreeMap::from([
            ("K".into(), "[1 2; 3 4]".into()),
            ("T".into(), "1e-6".into()),
        ]);
        assert_eq!(
            parse_workspace(&workspace_text(&values).unwrap()).unwrap(),
            values
        );
        for value in ["[1\n2]", "1;"] {
            assert!(workspace_text(&BTreeMap::from([("x".into(), value.into())])).is_err());
        }
        assert_eq!(workspace_text(&BTreeMap::new()).unwrap(), "");
    }
}
