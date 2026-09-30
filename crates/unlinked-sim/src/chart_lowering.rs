//! Lower pure scalar MATLAB Function charts without flattening their generated backing systems.
use super::matlab_function::prepare;
use super::{block_error, Error};
use std::collections::BTreeMap;
use unlinked_matlab::ArrayBudget;

/// Replace matched MATLAB Function backing subsystems before ordinary flattening.
/// Escaped chart paths are matched structurally; duplicate names are ambiguous.
pub(super) fn lower_charts(
    mut model: unlinked_model::Model,
) -> Result<unlinked_model::Model, Error> {
    use std::collections::BTreeSet;
    use unlinked_model::{ChartKind, DataScope, StateKind, System};
    if model.charts.is_empty() {
        return Ok(model);
    }
    let mut chart_ids = BTreeSet::new();
    let mut charts = BTreeMap::new();
    let mut total_bytes = 0usize;
    for chart in &model.charts {
        if !chart_ids.insert(&chart.id) {
            return Err(block_error(&chart.name, "duplicate chart ID"));
        }
        if charts.insert(chart.name.as_str(), chart).is_some() {
            return Err(block_error(&chart.name, "ambiguous duplicate chart path"));
        }
        total_bytes = total_bytes.saturating_add(chart.script.as_ref().map_or(0, String::len));
    }
    if charts.len() > 1024 || total_bytes > 1_048_576 {
        return Err(Error::Options(
            "MATLAB Function charts exceed 1024 charts or 1 MiB source".into(),
        ));
    }
    fn walk(
        system: &mut System,
        prefix: &str,
        depth: usize,
        charts: &BTreeMap<&str, &unlinked_model::Chart>,
        seen: &mut BTreeSet<String>,
        work: &mut usize,
        budget: &mut ArrayBudget,
    ) -> Result<(), Error> {
        if depth > 64 {
            return Err(Error::Options(
                "MATLAB Function chart path exceeds 64 subsystem levels".into(),
            ));
        }
        let mut siblings = BTreeSet::new();
        for block in &mut system.blocks {
            *work = work.checked_sub(1).ok_or_else(|| {
                Error::Options("MATLAB Function path traversal exceeds 100000 blocks".into())
            })?;
            let name = block.name.replace('/', "//");
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if !siblings.insert(block.name.clone()) {
                return Err(block_error(
                    &block.id.0,
                    "ambiguous duplicate block name in chart path",
                ));
            }
            if let Some(chart) = charts.get(path.as_str()) {
                if !seen.insert(path.clone()) {
                    return Err(block_error(&block.id.0, "ambiguous chart path"));
                }
                if chart.kind != ChartKind::MatlabFunction {
                    return Err(block_error(&block.id.0,"only pure MATLAB Function charts are supported; Stateflow state charts are not executable"));
                }
                if chart
                    .update_method
                    .as_deref()
                    .is_some_and(|s| s.trim() != "INHERITED")
                    || chart
                        .sample_time
                        .as_deref()
                        .is_some_and(|s| !["-1", "0"].contains(&s.trim()))
                {
                    return Err(block_error(
                        &block.id.0,
                        "explicit MATLAB Function chart scheduling is unsupported",
                    ));
                }
                if !pure_wrapper_flow(chart) || chart.states.len() > 1 {
                    return Err(block_error(
                        &block.id.0,
                        "MATLAB Function chart contains unsupported stateful graphical content",
                    ));
                }
                let script = chart.script.as_deref().ok_or_else(|| {
                    block_error(&block.id.0, "MATLAB Function chart has no script")
                })?;
                if chart.states.iter().any(|state| {
                    state.kind != StateKind::Function
                        || state.parent.is_some()
                        || state.subviewer.as_deref().is_some_and(|id| id != chart.id)
                        || state.script.as_deref() != Some(script)
                }) {
                    return Err(block_error(
                        &block.id.0,
                        "MATLAB Function chart has unsupported state semantics",
                    ));
                }
                let program = prepare(&block.id.0, script, block.ports.inputs as usize)?;
                if block.ports.outputs != 1 {
                    return Err(block_error(
                        &block.id.0,
                        "MATLAB Function block must declare exactly one output port",
                    ));
                }
                let signature = program.signature();
                validate_backing(block, signature)?;
                if chart.data.len() > 1024 {
                    return Err(block_error(
                        &block.id.0,
                        "MATLAB Function chart exceeds 1024 data declarations",
                    ));
                }
                let mut data_names = BTreeSet::new();
                let mut data_ids = BTreeSet::new();
                let mut inputs = BTreeMap::new();
                let mut outputs = BTreeMap::new();
                for data in &chart.data {
                    if !data_names.insert(data.name.as_str()) || !data_ids.insert(data.id.as_str())
                    {
                        return Err(block_error(&block.id.0, "duplicate chart data name or ID"));
                    }
                    if data
                        .variable_size
                        .as_deref()
                        .is_some_and(|s| !["0", "off"].contains(&s.trim()))
                    {
                        return Err(block_error(
                            &block.id.0,
                            "variable-size chart data is unsupported",
                        ));
                    }
                    if data.complexity.as_deref().is_some_and(|s| {
                        ![
                            "real",
                            "SF_REAL",
                            "SF_COMPLEX_NO",
                            "SF_COMPLEX_INHERITED",
                            "Inherited",
                        ]
                        .contains(&s.trim())
                    }) {
                        return Err(block_error(
                            &block.id.0,
                            "complex chart data is unsupported",
                        ));
                    }
                    if data.data_type.as_deref().is_some_and(|s| {
                        ![
                            "double",
                            "Inherit: Same as Simulink",
                            "Inherit: Same as input",
                        ]
                        .contains(&s.trim())
                    }) {
                        return Err(block_error(
                            &block.id.0,
                            "non-double chart data type is unsupported",
                        ));
                    }
                    if let Some(size) = &data.size {
                        let dims = unlinked_matlab::eval_array_expr_with_budget(
                            size,
                            &BTreeMap::new(),
                            budget,
                        )
                        .map_err(|e| {
                            block_error(&block.id.0, format!("chart data dimensions: {e}"))
                        })?;
                        let inherited = dims.data.len() == 1 && dims.data[0] == -1.;
                        if !inherited
                            && (dims.data.is_empty()
                                || dims.data.len() > 2
                                || dims.data.iter().any(|n| *n != 1.))
                        {
                            return Err(block_error(
                                &block.id.0,
                                "non-scalar chart data is unsupported",
                            ));
                        }
                    }
                    let ports=match data.scope {DataScope::Input=>Some(&mut inputs),DataScope::Output=>Some(&mut outputs),DataScope::Local=>None,_=>return Err(block_error(&block.id.0,"chart parameters, constants and external data require unsupported workspace semantics"))};
                    if let Some(ports) = ports {
                        let port = data.port.ok_or_else(|| {
                            block_error(&block.id.0, "chart input/output lacks port metadata")
                        })?;
                        if ports.insert(port, data.name.as_str()).is_some() {
                            return Err(block_error(&block.id.0, "duplicate chart data port"));
                        }
                    }
                }
                for (declared, names) in
                    [(&inputs, &signature.inputs), (&outputs, &signature.outputs)]
                {
                    if declared.len() != names.len()
                        || names.iter().enumerate().any(|(i, name)| {
                            declared.get(&(i as u32 + 1)).copied() != Some(name.as_str())
                        })
                    {
                        return Err(block_error(
                            &block.id.0,
                            "chart data names and ports must match function signature in order",
                        ));
                    }
                }
                block.block_type = "MatlabFunction".into();
                block.parameters.insert("Script".into(), script.into());
                block.subsystem = None;
            } else if let Some(sub) = &mut block.subsystem {
                walk(sub, &path, depth + 1, charts, seen, work, budget)?;
            }
        }
        Ok(())
    }

    let mut seen = BTreeSet::new();
    let mut work = 100_000;
    walk(
        &mut model.root,
        "",
        0,
        &charts,
        &mut seen,
        &mut work,
        &mut ArrayBudget::default(),
    )?;
    if seen.len() != charts.len() {
        return Err(Error::Options(
            "chart path does not match a unique model block".into(),
        ));
    }
    Ok(model)
}

/// EML subsystems contain a generated `sf_sfun` wrapper. Check the boundary
/// mapping before replacing it so edited wiring/extra executable blocks cannot
/// disappear silently during lowering.
fn validate_backing(
    block: &unlinked_model::Block,
    signature: &unlinked_matlab::FunctionSignature,
) -> Result<(), Error> {
    use std::collections::BTreeSet;
    use unlinked_model::{Block, PortKind};
    let fail = |message: &str| block_error(&block.id.0, message);
    fn ordinary(block: &Block) -> bool {
        block.mask.is_none()
            && block.library_source.is_none()
            && block.ports.enable == 0
            && block.ports.trigger == 0
            && block.ports.reset == 0
            && block.ports.ifaction == 0
            && block.ports.state == 0
            && block.ports.lconn == 0
            && block.ports.rconn == 0
            && block.param("Commented").is_none_or(|s| s == "off")
    }
    if block.block_type != "SubSystem"
        || block.param("SFBlockType") != Some("MATLAB Function")
        || !ordinary(block)
    {
        return Err(fail(
            "MATLAB Function chart requires an unmasked, unconditional MATLAB Function subsystem",
        ));
    }
    if block
        .param("SystemSampleTime")
        .is_some_and(|s| s.trim() != "-1")
    {
        return Err(fail(
            "MATLAB Function backing SystemSampleTime must be inherited (-1)",
        ));
    }
    let system = block
        .subsystem
        .as_deref()
        .ok_or_else(|| fail("MATLAB Function backing subsystem is missing"))?;
    if system.blocks.len() > 1024 {
        return Err(fail(
            "MATLAB Function backing subsystem exceeds 1024 blocks",
        ));
    }
    let mut blocks = BTreeMap::new();
    let mut inports = BTreeMap::new();
    let mut outports = BTreeMap::new();
    let mut sfun = None;
    for child in &system.blocks {
        if blocks.insert(&child.id, child).is_some()
            || !ordinary(child)
            || child.subsystem.is_some()
        {
            return Err(fail("MATLAB Function backing subsystem has duplicate IDs or unsupported nested/masked/conditional content"));
        }
        match child.block_type.as_str() {
            "Inport" | "Outport" => {
                for (key, values) in [
                    ("SampleTime", &["-1", "0"][..]),
                    ("PortDimensions", &["-1", "1", "[1 1]"][..]),
                    (
                        "OutDataTypeStr",
                        &["double", "Inherit: auto", "Inherit: Same as input"][..],
                    ),
                    ("SignalType", &["real", "auto"][..]),
                    ("VarSizeSig", &["No", "Inherit"][..]),
                ] {
                    if child
                        .param(key)
                        .is_some_and(|v| !values.contains(&v.trim()))
                    {
                        return Err(fail("MATLAB Function backing port has unsupported dimensions, timing or data type"));
                    }
                }
                let port = child
                    .param("Port")
                    .unwrap_or("1")
                    .parse::<u32>()
                    .map_err(|_| fail("invalid MATLAB Function backing port number"))?;
                let ports = if child.block_type == "Inport" {
                    &mut inports
                } else {
                    &mut outports
                };
                if ports.insert(port, child).is_some() {
                    return Err(fail("duplicate MATLAB Function backing port"));
                }
            }
            "S-Function" if child.param("FunctionName") == Some("sf_sfun") => {
                if sfun.replace(child).is_some() {
                    return Err(fail("multiple MATLAB Function backing S-functions"));
                }
            }
            "Demux" | "Terminator" => {}
            _ => {
                return Err(fail(
                    "MATLAB Function backing subsystem contains additional executable content",
                ))
            }
        }
    }
    let sfun = sfun.ok_or_else(|| fail("MATLAB Function backing sf_sfun is missing"))?;
    if sfun.ports.inputs as usize != signature.inputs.len() || sfun.ports.outputs != 2 {
        return Err(fail(
            "unsupported MATLAB Function backing S-function port counts",
        ));
    }
    let mut expected = BTreeSet::new();
    for (ports, names, input) in [
        (&inports, &signature.inputs, true),
        (&outports, &signature.outputs, false),
    ] {
        if ports.len() != names.len() {
            return Err(fail("MATLAB Function backing ports differ from signature"));
        }
        for (index, name) in names.iter().enumerate() {
            let port = index as u32 + 1;
            let child = ports
                .get(&port)
                .ok_or_else(|| fail("missing MATLAB Function backing port index"))?;
            if child.name != *name {
                return Err(fail(
                    "MATLAB Function backing port name differs from signature",
                ));
            }
            expected.insert(if input {
                (child.id.clone(), 1, sfun.id.clone(), port)
            } else {
                (sfun.id.clone(), 2, child.id.clone(), 1)
            });
        }
    }
    for connection in system.connections() {
        let src = blocks
            .get(&connection.src.block)
            .ok_or_else(|| fail("missing MATLAB Function backing wire source"))?;
        let dst = blocks
            .get(&connection.dst.block)
            .ok_or_else(|| fail("missing MATLAB Function backing wire destination"))?;
        if connection.src.port.kind != PortKind::Out || connection.dst.port.kind != PortKind::In {
            return Err(fail("unsupported MATLAB Function backing wire kind"));
        }
        let key = (
            connection.src.block.clone(),
            connection.src.port.index,
            connection.dst.block.clone(),
            connection.dst.port.index,
        );
        if expected.remove(&key) {
            continue;
        }
        let dummy =
            (src.id == sfun.id && connection.src.port.index == 1 && dst.block_type == "Demux")
                || (src.block_type == "Demux"
                    && dst.block_type == "Terminator"
                    && connection.src.port.index <= src.ports.outputs);
        if !dummy || connection.dst.port.index != 1 || connection.src.port.index == 0 {
            return Err(fail(
                "MATLAB Function backing wires differ from generated wrapper",
            ));
        }
    }
    if !expected.is_empty() {
        return Err(fail("MATLAB Function backing input/output is disconnected"));
    }
    Ok(())
}

/// Older EML charts encode one default transition invoking the generated
/// kernel, ending at a connective junction. This wrapper has no user state.
fn pure_wrapper_flow(chart: &unlinked_model::Chart) -> bool {
    use unlinked_model::JunctionKind;
    if chart.transitions.is_empty() && chart.junctions.is_empty() {
        return true;
    }
    let ([transition], [junction], [state]) = (
        chart.transitions.as_slice(),
        chart.junctions.as_slice(),
        chart.states.as_slice(),
    ) else {
        return false;
    };
    let top = |view: &Option<String>| view.as_deref().is_none_or(|v| v == chart.id);
    transition.src.is_none()
        && transition.dst.as_deref() == Some(junction.id.as_str())
        && transition.label.split_whitespace().collect::<String>() == "{eML_blk_kernel();}"
        && state.label.trim() == "eML_blk_kernel()"
        && junction.kind == JunctionKind::Connective
        && top(&transition.subviewer)
        && top(&junction.subviewer)
}
