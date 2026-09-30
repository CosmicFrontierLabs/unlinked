use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::{compile, simulate_model, Error, Options, Solver};
fn block(id: &str, kind: &str, params: &[(&str, &str)]) -> Block {
    Block {
        id: id.into(),
        name: id.into(),
        block_type: kind.into(),
        position: Rect::default(),
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts::default(),
        parameters: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        mask: None,
        library_source: None,
        subsystem: None,
        style: BlockStyle::default(),
    }
}
fn line(source: &str, target: &str, input: u32) -> Line {
    Line {
        src: Some(Endpoint {
            block: source.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        }),
        dst: Some(Endpoint {
            block: target.into(),
            port: PortRef {
                kind: PortKind::In,
                index: input,
            },
        }),
        ..Line::default()
    }
}
fn model(numerator: &str, denominator: &str) -> Model {
    Model {
        charts: Vec::new(),
        name: "response".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        workspace: BTreeMap::new(),
        root: System {
            blocks: vec![
                block("source", "Constant", &[("Value", "1")]),
                block(
                    "tf",
                    "TransferFcn",
                    &[("Numerator", numerator), ("Denominator", denominator)],
                ),
            ],
            lines: vec![line("source", "tf", 1)],
            ..System::default()
        },
    }
}
fn options(solver: Solver) -> Options {
    Options {
        stop: 2.0,
        step: 0.01,
        solver,
        relative_tolerance: 1e-10,
        absolute_tolerance: 1e-12,
        ..Options::default()
    }
}

fn state_model(params: &[(&str, &str)]) -> Model {
    let mut m = model("1", "1");
    m.root.blocks[1] = block("tf", "StateSpace", params);
    m
}
#[test]
fn coupled_three_state_realization_matches_polynomial_solution() {
    // x1'=x2, x2'=x3, x3'=1, with nonzero initial state.
    let m = state_model(&[
        ("A", "[0 1 0;0 0 1;0 0 0]"),
        ("B", "[0;0;1]"),
        ("C", "[1 2 3]"),
        ("D", ".5"),
        ("InitialCondition", "[1 2 3]"),
    ]);
    for solver in [Solver::Rk4, Solver::Rk45] {
        let trace = simulate_model(&m, &options(solver)).unwrap();
        for (i, &t) in trace.time.iter().enumerate() {
            let x1 = 1. + 2. * t + 1.5 * t * t + t * t * t / 6.;
            let x2 = 2. + 3. * t + t * t / 2.;
            let x3 = 3. + t;
            assert!((trace.signals["tf"][i] - (x1 + 2. * x2 + 3. * x3 + 0.5)).abs() < 1e-9);
        }
    }
}
#[test]
fn matrix_workspace_and_initial_alias_match_decaying_solution() {
    let mut m = state_model(&[
        ("A", "a"),
        ("B", "zeros(3,1)"),
        ("C", "[1 2 3]"),
        ("D", "0"),
        ("X0", "[.1;.2;.3]"),
    ]);
    m.workspace
        .insert("a".into(), "[-1 0 0;0 -2 0;0 0 -3]".into());
    let trace = simulate_model(&m, &options(Solver::Rk45)).unwrap();
    for (i, &t) in trace.time.iter().enumerate() {
        let expected = 0.1 * (-t).exp() + 0.4 * (-2. * t).exp() + 0.9 * (-3. * t).exp();
        assert!((trace.signals["tf"][i] - expected).abs() < 1e-9);
    }
}
#[test]
fn defaults_include_direct_feedthrough_and_64_states_are_supported() {
    let trace = simulate_model(&state_model(&[]), &options(Solver::Rk45)).unwrap();
    for (i, &t) in trace.time.iter().enumerate() {
        assert!((trace.signals["tf"][i] - t.exp()).abs() < 1e-8);
    }
    compile(
        &state_model(&[
            ("A", "eye(64)"),
            ("B", "ones(64,1)"),
            ("C", "ones(1,64)"),
            ("D", "0"),
        ]),
        &Options::default(),
    )
    .unwrap();
}
#[test]
fn unsupported_dimensions_nonfinite_and_tuning_reject() {
    for (key, value) in [
        ("A", "ones(2,3)"),
        ("A", "eye(65)"),
        ("A", "Inf"),
        ("B", "[1 2]"),
        ("C", "[1;2]"),
        ("D", "[1 2]"),
        ("InitialCondition", "[1 2]"),
        ("AllowTunableDMatrix", "on"),
        ("AbsoluteTolerance", "0.01"),
    ] {
        assert!(
            compile(&state_model(&[(key, value)]), &Options::default()).is_err(),
            "accepted {key}={value}"
        );
    }
}
#[test]
fn zero_d_preserves_dynamic_feedback_but_nonzero_d_rejects_loop() {
    let mut m = state_model(&[("A", "-1"), ("D", "0")]);
    m.root.lines = vec![line("tf", "tf", 1)];
    compile(&m, &Options::default()).unwrap();
    m.root.blocks[1].parameters.insert("D".into(), "1".into());
    assert!(matches!(
        compile(&m, &Options::default()),
        Err(Error::AlgebraicLoop(_))
    ));
}
