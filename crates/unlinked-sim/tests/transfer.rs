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
        name: "response".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        workspace: BTreeMap::new(),
        charts: Vec::new(),
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
#[test]
fn first_through_fourth_order_step_responses_match_analytic_solutions() {
    type Case = (&'static str, &'static str, fn(f64) -> f64);
    let cases: &[Case] = &[
        ("[1]", "[1 3 3 1]", |t| {
            1.0 - (-t).exp() * (1.0 + t + t * t / 2.0)
        }),
        ("[1]", "[1 4 6 4 1]", |t| {
            1.0 - (-t).exp() * (1.0 + t + t * t / 2.0 + t * t * t / 6.0)
        }),
        ("[1]", "[1 0 0 0]", |t| t * t * t / 6.0),
        ("[1]", "[1 1]", |t| 1.0 - (-t).exp()),
        ("[2]", "[2 4]", |t| 0.5 * (1.0 - (-2.0 * t).exp())),
        ("[1]", "[1 0 1]", |t| 1.0 - t.cos()),
        ("[1 2]", "[1 1]", |t| 2.0 - (-t).exp()),
        ("[1 0 0]", "[1 2 1]", |t| (1.0 - t) * (-t).exp()),
        ("[6]", "[2]", |_| 3.0),
        ("[0]", "[1 2 1]", |_| 0.0),
    ];
    for solver in [Solver::Rk4, Solver::Rk45] {
        for &(numerator, denominator, exact) in cases {
            let trace = simulate_model(&model(numerator, denominator), &options(solver)).unwrap();
            for (i, &time) in trace.time.iter().enumerate() {
                assert!(
                    (trace.signals["tf"][i] - exact(time)).abs() < 2e-8,
                    "{numerator}/{denominator} at {time}: {} != {}",
                    trace.signals["tf"][i],
                    exact(time)
                );
            }
        }
    }
}
#[test]
fn strict_proper_feedback_is_dynamic_but_feedthrough_loop_rejects() {
    let mut model = model("[1]", "[1 1]");
    model.root.blocks[0] = block("source", "Sum", &[("Inputs", "+-")]);
    model
        .root
        .blocks
        .push(block("constant", "Constant", &[("Value", "1")]));
    model
        .root
        .lines
        .extend([line("constant", "source", 1), line("tf", "source", 2)]);
    let trace = simulate_model(&model, &options(Solver::Rk45)).unwrap();
    for (i, &t) in trace.time.iter().enumerate() {
        assert!((trace.signals["tf"][i] - 0.5 * (1.0 - (-2.0 * t).exp())).abs() < 1e-9);
    }
    model.root.blocks[1]
        .parameters
        .insert("Numerator".into(), "[1 2]".into());
    assert!(matches!(
        compile(&model, &options(Solver::Rk45)),
        Err(Error::AlgebraicLoop(_))
    ));
}
#[test]
fn scalar_workspace_expressions_are_allowed_inside_coefficient_rows() {
    let mut m = model("[gain / 2]", "[tau, min(1, gain)]");
    m.workspace = BTreeMap::from([("gain".into(), "2".into()), ("tau".into(), "1".into())]);
    let trace = simulate_model(&m, &options(Solver::Rk45)).unwrap();
    assert!((trace.signals["tf"].last().unwrap() - (1.0 - (-2.0_f64).exp())).abs() < 1e-9);
}
#[test]
fn unsupported_and_ambiguous_coefficient_semantics_reject() {
    for (num, den) in [
        ("[1;2]", "[1 1]"),
        ("[1 2 3]", "[1 1]"),
        ("[1]", "[0 1]"),
        ("[1]", "ones(1,66)"),
        ("[]", "[1 1]"),
        ("[1,,2]", "[1 1]"),
        ("[Inf]", "[1 1]"),
    ] {
        assert!(
            compile(&model(num, den), &Options::default()).is_err(),
            "accepted {num}/{den}"
        );
    }
    let mut m = model("[1]", "[1 1]");
    m.root.blocks[1]
        .parameters
        .insert("AbsoluteTolerance".into(), "0.1".into());
    assert!(compile(&m, &Options::default()).is_err());
}

fn discrete(num: &str, den: &str, initial: &str) -> Model {
    let mut m = model(num, den);
    m.root.blocks[1].block_type = "DiscreteTransferFcn".into();
    m.root.blocks[1]
        .parameters
        .insert("InitialStates".into(), initial.into());
    m
}
#[test]
fn discrete_recurrences_and_raw_denominator_initial_states() {
    // Direct-form-II states preserve the unnormalized denominator, as in Simulink.
    for (num, den, initial, expected) in [
        ("1", "[1 -.5]", "0", vec![0., 1., 1.5, 1.75]),
        ("[1 2]", "[1 -.5]", "0", vec![1., 3.5, 4.75, 5.375]),
        ("1", "[1 0 0]", "0", vec![0., 0., 1., 1.]),
        ("4", "[2 3]", "2", vec![8., -10., 17., -23.5]),
        ("[2 3]", "[1 0 0]", "[4 5]", vec![23., 14., 5., 5.]),
    ] {
        for solver in [Solver::Euler, Solver::Rk4, Solver::Rk45] {
            let trace = simulate_model(
                &discrete(num, den, initial),
                &Options {
                    stop: 0.3,
                    step: 0.1,
                    solver,
                    ..Options::default()
                },
            )
            .unwrap();
            for (actual, expected) in trace.signals["tf"].iter().zip(&expected) {
                assert!(
                    (actual - expected).abs() < 1e-10,
                    "{num}/{den}: {actual} != {expected}"
                );
            }
        }
    }
}
#[test]
fn discrete_feedthrough_into_continuous_state_requires_hold() {
    let mut m = discrete("[1 2]", "[1 1]", "0");
    m.root.blocks[0] = block("source", "Clock", &[]);
    m.root.blocks.push(block("integrator", "Integrator", &[]));
    m.root.lines.push(line("tf", "integrator", 1));
    let error = compile(&m, &Options::default()).err().unwrap().to_string();
    assert!(
        error.contains("without an explicit UnitDelay hold"),
        "{error}"
    );
    m.root.blocks[1]
        .parameters
        .insert("Numerator".into(), "1".into());
    compile(&m, &Options::default()).unwrap();
    m.root.blocks[1]
        .parameters
        .insert("Numerator".into(), "[1 2]".into());
    m.root.blocks.push(block("hold", "UnitDelay", &[]));
    m.root.lines.pop();
    m.root
        .lines
        .extend([line("tf", "hold", 1), line("hold", "integrator", 1)]);
    compile(&m, &Options::default()).unwrap();
}
#[test]
fn order_64_is_bounded_and_discrete_extensions_reject() {
    compile(&model("1", "[1 zeros(1,64)]"), &Options::default()).unwrap();
    for (key, value) in [
        ("InitialStates", "[1 2 3]"),
        ("ExternalReset", "rising"),
        ("NumeratorSource", "Input port"),
        ("StateDataTypeStr", "single"),
        ("InputProcessing", "Columns as channels (frame based)"),
    ] {
        let mut m = discrete("1", "[1 1]", "0");
        m.root.blocks[1].parameters.insert(key.into(), value.into());
        assert!(
            compile(&m, &Options::default()).is_err(),
            "accepted {key}={value}"
        );
    }
}
