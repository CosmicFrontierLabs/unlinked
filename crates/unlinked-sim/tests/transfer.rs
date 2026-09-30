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
fn first_and_second_order_step_responses_match_analytic_solutions() {
    type Case = (&'static str, &'static str, fn(f64) -> f64);
    let cases: &[Case] = &[
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
    // Whitespace within one coefficient requires comma-separated notation;
    // unbracketed scalar expression is unambiguous and accepted.
    m.root.blocks[1]
        .parameters
        .insert("Numerator".into(), "gain / 2".into());
    let trace = simulate_model(&m, &options(Solver::Rk45)).unwrap();
    assert!((trace.signals["tf"].last().unwrap() - (1.0 - (-2.0_f64).exp())).abs() < 1e-9);
}
#[test]
fn unsupported_and_ambiguous_coefficient_semantics_reject() {
    for (num, den) in [
        ("[1;2]", "[1 1]"),
        ("[1 2 3]", "[1 1]"),
        ("[1]", "[0 1]"),
        ("[1]", "[1 2 3 4]"),
        ("[]", "[1 1]"),
        ("[1,,2]", "[1 1]"),
        ("[1,]", "[1 1]"),
        ("[1 + 2]", "[1 1]"),
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
