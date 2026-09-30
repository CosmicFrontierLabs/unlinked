use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::{compile, simulate_model, Options, Solver};
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
fn line(source: &str, output: u32, target: &str, input: u32) -> Line {
    Line {
        src: Some(Endpoint {
            block: source.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: output,
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
fn model(blocks: Vec<Block>, lines: Vec<Line>) -> Model {
    Model {
        name: "vectors".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        workspace: BTreeMap::new(),
        root: System {
            blocks,
            lines,
            ..System::default()
        },
    }
}
fn options() -> Options {
    Options {
        step: 0.1,
        stop: 0.9,
        ..Options::default()
    }
}
fn close(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!((a - b).abs() < 1e-12, "sample {i}: {a} != {b}");
    }
}
#[test]
fn independent_delays_capture_at_hits_and_hold_until_next_hit() {
    let m = model(
        vec![
            block("clock", "Clock", &[]),
            block(
                "a",
                "UnitDelay",
                &[("InitialCondition", "-1"), ("SampleTime", "0.2")],
            ),
            block(
                "b",
                "UnitDelay",
                &[("InitialCondition", "-2"), ("SampleTime", "0.3")],
            ),
        ],
        vec![line("clock", 1, "a", 1), line("clock", 1, "b", 1)],
    );
    let t = simulate_model(&m, &options()).unwrap();
    close(
        &t.signals["a"],
        &[-1., -1., 0., 0., 0.2, 0.2, 0.4, 0.4, 0.6, 0.6],
    );
    close(
        &t.signals["b"],
        &[-2., -2., -2., 0., 0., 0., 0.3, 0.3, 0.3, 0.6],
    );
}
#[test]
fn different_rate_feedback_updates_simultaneously() {
    let m = model(
        vec![
            block(
                "a",
                "UnitDelay",
                &[("InitialCondition", "1"), ("SampleTime", "0.2")],
            ),
            block(
                "b",
                "UnitDelay",
                &[("InitialCondition", "2"), ("SampleTime", "0.3")],
            ),
        ],
        vec![line("a", 1, "b", 1), line("b", 1, "a", 1)],
    );
    let t = simulate_model(&m, &options()).unwrap();
    close(&t.signals["a"], &[1., 1., 2., 2., 2., 2., 1., 1., 2., 2.]);
    close(&t.signals["b"], &[2., 2., 2., 1., 1., 1., 2., 2., 2., 1.]);
}
#[test]
fn discrete_transfer_recurrence_and_direct_output_hold() {
    let m = model(
        vec![
            block("c", "Constant", &[("Value", "1")]),
            block("clock", "Clock", &[]),
            block(
                "strict",
                "DiscreteTransferFcn",
                &[
                    ("Numerator", "[1]"),
                    ("Denominator", "[1 -0.5]"),
                    ("SampleTime", "0.3"),
                ],
            ),
            block(
                "direct",
                "DiscreteTransferFcn",
                &[
                    ("Numerator", "[1 0]"),
                    ("Denominator", "[1 -0.5]"),
                    ("SampleTime", "0.3"),
                ],
            ),
            block(
                "identity",
                "DiscreteTransferFcn",
                &[
                    ("Numerator", "1"),
                    ("Denominator", "1"),
                    ("SampleTime", "0.3"),
                ],
            ),
        ],
        vec![
            line("c", 1, "strict", 1),
            line("c", 1, "direct", 1),
            line("clock", 1, "identity", 1),
        ],
    );
    let t = simulate_model(&m, &options()).unwrap();
    close(
        &t.signals["strict"],
        &[0., 0., 0., 1., 1., 1., 1.5, 1.5, 1.5, 1.75],
    );
    close(
        &t.signals["direct"],
        &[1., 1., 1., 1.5, 1.5, 1.5, 1.75, 1.75, 1.75, 1.875],
    );
    close(
        &t.signals["identity"],
        &[0., 0., 0., 0.3, 0.3, 0.3, 0.6, 0.6, 0.6, 0.9],
    );
}
#[test]
fn digital_clock_holds_through_every_continuous_solver_stage() {
    let m = model(
        vec![
            block("clock", "DigitalClock", &[("SampleTime", "0.3")]),
            block("i", "Integrator", &[]),
        ],
        vec![line("clock", 1, "i", 1)],
    );
    for solver in [Solver::Euler, Solver::Rk4, Solver::Rk45] {
        let t = simulate_model(
            &m,
            &Options {
                solver,
                ..options()
            },
        )
        .unwrap();
        close(
            &t.signals["clock"],
            &[0., 0., 0., 0.3, 0.3, 0.3, 0.6, 0.6, 0.6, 0.9],
        );
        close(
            &t.signals["i"],
            &[0., 0., 0., 0., 0.03, 0.06, 0.09, 0.15, 0.21, 0.27],
        );
    }
}
#[test]
fn nonzero_aligned_start_and_decimal_rounding() {
    let m = model(
        vec![block("clock", "DigitalClock", &[("SampleTime", "0.3")])],
        vec![],
    );
    let t = simulate_model(
        &m,
        &Options {
            start: 0.6,
            stop: 1.2,
            ..options()
        },
    )
    .unwrap();
    close(&t.signals["clock"], &[0.6, 0.6, 0.6, 0.9, 0.9, 0.9, 1.2]);
    assert!(simulate_model(
        &m,
        &Options {
            start: 0.1,
            ..options()
        }
    )
    .unwrap_err()
    .to_string()
    .contains("zero-phase"));
    assert!(simulate_model(
        &m,
        &Options {
            stop: 0.95,
            ..options()
        }
    )
    .is_err());
}
#[test]
fn invalid_sample_periods_and_direct_discrete_algebraic_loop_reject() {
    for period in ["0", "0.15", "-2", "[0.2 0.1]", "1e20"] {
        let m = model(
            vec![block("clock", "DigitalClock", &[("SampleTime", period)])],
            vec![],
        );
        assert!(compile(&m, &options()).is_err(), "{period}");
    }
    let m = model(
        vec![block(
            "d",
            "DiscreteTransferFcn",
            &[
                ("Numerator", "1"),
                ("Denominator", "1"),
                ("SampleTime", "0.2"),
            ],
        )],
        vec![line("d", 1, "d", 1)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("algebraic"));
}

#[test]
fn inherited_slower_sample_rate_is_not_silently_assigned_output_step() {
    let m = model(
        vec![
            block("clock", "DigitalClock", &[("SampleTime", "0.3")]),
            block("delay", "UnitDelay", &[]),
        ],
        vec![line("clock", 1, "delay", 1)],
    );
    let error = compile(&m, &options()).unwrap_err().to_string();
    assert!(error.contains("inherited sample-rate propagation"));
}
