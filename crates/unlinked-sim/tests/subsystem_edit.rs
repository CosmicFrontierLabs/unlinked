//! Grouping native blocks into a virtual subsystem must preserve execution.
use unlinked_model::{catalog, edit, *};
use unlinked_sim::{simulate_model, Options, Solver, Trace};

fn block(id: &str, kind: &str, overrides: &[(&str, &str)]) -> Block {
    let descriptor = catalog::find(kind).unwrap();
    let mut parameters = descriptor.creation_parameters();
    parameters.extend(overrides.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    let catalog::PortResolution::Known(ports) = descriptor.resolve_ports(&parameters) else {
        panic!("unresolved fixture ports: {kind}");
    };
    Block {
        id: id.into(),
        name: format!("{kind}{id}"),
        block_type: kind.into(),
        position: Rect::new(0., 0., 40., 30.),
        orientation: Orientation::Right,
        mirrored: false,
        ports,
        parameters,
        mask: None,
        library_source: None,
        subsystem: None,
        style: Default::default(),
        interface: None,
    }
}
fn input(id: &str, index: u32) -> Endpoint {
    Endpoint {
        block: id.into(),
        port: PortRef {
            kind: PortKind::In,
            index,
        },
    }
}
fn wire(src: &str, dst: &str, port: u32) -> Line {
    Line {
        src: Some(Endpoint {
            block: src.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        }),
        dst: Some(input(dst, port)),
        ..Default::default()
    }
}
fn branch(dst: &str, port: u32) -> Branch {
    Branch {
        dst: Some(input(dst, port)),
        ..Default::default()
    }
}
fn model(blocks: Vec<Block>, lines: Vec<Line>) -> Model {
    Model {
        name: "grouping".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: Default::default(),
        workspace: Default::default(),
        charts: vec![],
        type_defaults: Default::default(),
        root: System {
            blocks,
            lines,
            ..Default::default()
        },
    }
}
fn assert_equivalent(mut model: Model, selection: &[&str], outputs: &[&str]) -> Trace {
    let options = Options {
        stop: 1.,
        step: 0.01,
        solver: Solver::Rk4,
        ..Default::default()
    };
    assert!(validation::validate_structure(&model).is_valid());
    let before = simulate_model(&model, &options).unwrap();
    let id = edit::next_sid(&model).unwrap().to_string();
    edit::apply_batch(
        &mut model,
        &[edit::Edit::CreateSubsystem {
            system: vec![],
            ids: selection.iter().map(|id| (*id).into()).collect(),
            id: id.as_str().into(),
            name: "Grouped".into(),
        }],
    )
    .unwrap();
    assert!(validation::validate_structure(&model).is_valid());
    assert!(model
        .root
        .block(&id.as_str().into())
        .unwrap()
        .subsystem
        .is_some());
    let after = simulate_model(&model, &options).unwrap();
    assert_eq!(before.time, after.time);
    for id in outputs {
        let expected = &before.signals[*id];
        let actual = &after.signals[*id];
        assert_eq!(expected.len(), actual.len());
        for ((&time, &expected), &actual) in before.time.iter().zip(expected).zip(actual) {
            assert!(
                actual.is_finite() && (actual - expected).abs() <= 1e-11 * expected.abs().max(1.),
                "output {id} at {time}, selection {selection:?}: {actual} != {expected}"
            );
        }
    }
    edit::apply_batch(
        &mut model,
        &[edit::Edit::ExpandSubsystem {
            system: vec![],
            id: id.as_str().into(),
        }],
    )
    .unwrap();
    assert!(validation::validate_structure(&model).is_valid());
    let expanded = simulate_model(&model, &options).unwrap();
    assert_eq!(before.time, expanded.time);
    for id in outputs {
        for (&expected, &actual) in before.signals[*id].iter().zip(&expanded.signals[*id]) {
            assert!(
                actual.is_finite() && (actual - expected).abs() <= 1e-11 * expected.abs().max(1.),
                "expanded output {id}, selection {selection:?}: {actual} != {expected}"
            );
        }
    }
    before
}

#[test]
fn grouping_preserves_continuous_feedback_across_both_boundary_directions() {
    // x' = 1 - x; x fans out to the root output and the feedback gain.
    let mut state = wire("3", "4", 1);
    state.branches.push(branch("5", 1));
    let model = model(
        vec![
            block("1", "Constant", &[("Value", "1")]),
            block("2", "Sum", &[("Inputs", "++")]),
            block("3", "Integrator", &[("InitialCondition", "0")]),
            block("4", "Gain", &[("Gain", "-1")]),
            block("5", "Outport", &[("Port", "1")]),
        ],
        vec![
            wire("1", "2", 1),
            wire("4", "2", 2),
            wire("2", "3", 1),
            state,
        ],
    );
    for selected in [&["2", "4"][..], &["3", "4"], &["2", "3", "4"]] {
        let trace = assert_equivalent(model.clone(), selected, &["5"]);
        for (&t, &x) in trace.time.iter().zip(&trace.signals["5"]) {
            assert!((x - (1. - (-t).exp())).abs() < 1e-8);
        }
    }
}

#[test]
fn grouping_preserves_mixed_nested_fanout_and_multiple_interface_ports() {
    // External source 1 feeds two selected blocks and an unselected output.
    // Selected gain 2 also feeds both an internal block and a root output.
    let mut source = wire("1", "2", 1);
    source.branches = vec![Branch {
        branches: vec![branch("4", 3), branch("6", 1)],
        ..Default::default()
    }];
    let mut gain = wire("2", "4", 1);
    gain.branches.push(branch("5", 1));
    let mut sum = wire("4", "7", 1);
    sum.branches.push(branch("9", 1));
    let model = model(
        vec![
            block("1", "Constant", &[("Value", "3")]),
            block("2", "Gain", &[("Gain", "2")]),
            block("3", "Gain", &[("Gain", "-1")]),
            block("4", "Sum", &[("Inputs", "+++")]),
            block("5", "Outport", &[("Port", "1")]),
            block("6", "Outport", &[("Port", "2")]),
            block("7", "Outport", &[("Port", "3")]),
            block("8", "Sin", &[]),
            block("9", "Outport", &[("Port", "4")]),
        ],
        vec![source, gain, wire("8", "3", 1), wire("3", "4", 2), sum],
    );
    let trace = assert_equivalent(model, &["2", "3", "4"], &["5", "6", "7", "9"]);
    for (i, &t) in trace.time.iter().enumerate() {
        assert_eq!(trace.signals["5"][i], 6.);
        assert_eq!(trace.signals["6"][i], 3.);
        assert!((trace.signals["7"][i] - (9. - t.sin())).abs() < 1e-12);
        assert_eq!(trace.signals["7"][i], trace.signals["9"][i]);
    }
}

#[test]
fn grouping_preserves_discrete_state_updates_and_held_outputs() {
    let mut sum = wire("2", "3", 1);
    sum.branches.push(branch("5", 1));
    let mut state = wire("3", "4", 1);
    state.branches.push(branch("6", 1));
    let model = model(
        vec![
            block("1", "Constant", &[("Value", "1")]),
            block("2", "Sum", &[("Inputs", "++")]),
            block(
                "3",
                "UnitDelay",
                &[("InitialCondition", "0"), ("SampleTime", "0.1")],
            ),
            block("4", "Gain", &[("Gain", "-0.5")]),
            block("5", "Outport", &[("Port", "1")]),
            block("6", "Outport", &[("Port", "2")]),
        ],
        vec![wire("1", "2", 1), wire("4", "2", 2), sum, state],
    );
    let trace = assert_equivalent(model, &["2", "3", "4"], &["5", "6"]);
    let values = &trace.signals["6"];
    assert!(values.windows(2).any(|v| v[0] != v[1]));
    for (i, pair) in values.windows(2).enumerate() {
        if (i + 1) % 10 != 0 {
            assert_eq!(pair[0], pair[1]);
        }
    }
}
