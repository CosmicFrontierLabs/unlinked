use unlinked_sim::{simulate, Graph, Kind, Node, Options, Solver, Wire};
fn gaussian(seed: u32, period_ticks: usize) -> Kind {
    Kind::RandomNumber {
        mean: 2.0,
        variance: 9.0,
        seed,
        period_ticks,
    }
}
fn node(id: &str, kind: Kind) -> Node {
    Node {
        id: id.into(),
        name: id.into(),
        kind,
    }
}
fn options(solver: Solver) -> Options {
    Options {
        stop: 1.0,
        step: 0.1,
        solver,
        ..Options::default()
    }
}
#[test]
fn streams_reset_are_independent_and_hold_through_solver_stages() {
    let graph = Graph {
        nodes: vec![
            node("a", gaussian(765493, 2)),
            node("b", gaussian(765493, 2)),
            node("other", gaussian(7, 2)),
            node("integral", Kind::Integrator { initial: 0.0 }),
        ],
        wires: vec![Wire {
            source: "a".into(),
            target: "integral".into(),
            input: 0,
        }],
    };
    let baseline = simulate(&graph, &options(Solver::Euler)).unwrap();
    assert_eq!(baseline.signals["a"], baseline.signals["b"]);
    assert_ne!(baseline.signals["a"], baseline.signals["other"]);
    for pair in baseline.signals["a"].chunks(2) {
        assert!(pair.iter().all(|x| *x == pair[0]));
    }
    for solver in [Solver::Euler, Solver::Rk4, Solver::Rk45] {
        let actual = simulate(&graph, &options(solver)).unwrap();
        assert_eq!(actual.signals["a"], baseline.signals["a"]);
        for (actual, expected) in actual.signals["integral"]
            .iter()
            .zip(&baseline.signals["integral"])
        {
            assert!((actual - expected).abs() < 1e-10);
        }
    }
    let mut reversed = graph.clone();
    reversed.nodes.reverse();
    assert_eq!(
        simulate(&reversed, &options(Solver::Rk4)).unwrap().signals["a"],
        baseline.signals["a"]
    );
}
#[test]
fn gaussian_and_uniform_moments_and_extreme_bounds() {
    let graph = Graph {
        nodes: vec![
            node("g", gaussian(0, 1)),
            node(
                "u",
                Kind::UniformRandomNumber {
                    minimum: -2.0,
                    maximum: 4.0,
                    seed: 0,
                    period_ticks: 1,
                },
            ),
            node(
                "extreme",
                Kind::UniformRandomNumber {
                    minimum: -f64::MAX,
                    maximum: f64::MAX,
                    seed: 0,
                    period_ticks: 1,
                },
            ),
            node(
                "zero",
                Kind::RandomNumber {
                    mean: 3.0,
                    variance: 0.0,
                    seed: 0,
                    period_ticks: 1,
                },
            ),
        ],
        wires: vec![],
    };
    let trace = simulate(
        &graph,
        &Options {
            stop: 50000.0,
            step: 1.0,
            ..Options::default()
        },
    )
    .unwrap();
    let g = &trace.signals["g"];
    let mean = g.iter().sum::<f64>() / g.len() as f64;
    let variance = g.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / g.len() as f64;
    assert!((mean - 2.0).abs() < 0.06, "mean={mean}");
    assert!((variance - 9.0).abs() < 0.2, "variance={variance}");
    let u = &trace.signals["u"];
    assert!(u.iter().all(|v| *v > -2.0 && *v < 4.0));
    assert!((u.iter().sum::<f64>() / u.len() as f64 - 1.0).abs() < 0.04);
    assert!(trace.signals["zero"].iter().all(|v| *v == 3.0));
}
#[test]
fn invalid_graph_parameters_reject() {
    for kind in [
        Kind::RandomNumber {
            mean: 0.0,
            variance: -1.0,
            seed: 0,
            period_ticks: 1,
        },
        Kind::RandomNumber {
            mean: f64::NAN,
            variance: 1.0,
            seed: 0,
            period_ticks: 1,
        },
        gaussian(0, 0),
        Kind::UniformRandomNumber {
            minimum: 2.0,
            maximum: 1.0,
            seed: 0,
            period_ticks: 1,
        },
        Kind::UniformRandomNumber {
            minimum: 1.0,
            maximum: 1.0,
            seed: 0,
            period_ticks: 1,
        },
    ] {
        assert!(simulate(
            &Graph {
                nodes: vec![node("bad", kind)],
                wires: vec![]
            },
            &options(Solver::Rk4)
        )
        .is_err());
    }
}

fn source_model(kind: &str, parameters: &[(&str, &str)]) -> unlinked_model::Model {
    use unlinked_model::*;
    Model {
        name: "random test".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        type_defaults: Default::default(),
        charts: vec![],
        workspace: Default::default(),
        root: System {
            blocks: vec![Block {
                id: "noise".into(),
                name: "Noise".into(),
                block_type: kind.into(),
                position: Rect::default(),
                orientation: Orientation::Right,
                mirrored: false,
                ports: PortCounts::default(),
                mask: None,
                library_source: None,
                subsystem: None,
                style: BlockStyle::default(),
                interface: None,
                parameters: parameters
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }],
            ..System::default()
        },
    }
}
#[test]
fn imported_parameters_defaults_and_invalid_values() {
    for kind in ["RandomNumber", "UniformRandomNumber"] {
        let graph = unlinked_sim::compile(&source_model(kind, &[]), &options(Solver::Rk4)).unwrap();
        assert!(simulate(&graph, &options(Solver::Rk4)).is_ok());
        for parameters in [
            vec![("Seed", "-1")],
            vec![("Seed", "1.5")],
            vec![("Seed", "4294967296")],
            vec![("Seed", "Inf")],
            vec![("SampleTime", "0")],
            vec![("SampleTime", "-1")],
            vec![("SampleTime", "0.15")],
        ] {
            assert!(
                unlinked_sim::compile(&source_model(kind, &parameters), &options(Solver::Rk4))
                    .is_err(),
                "{kind}: {parameters:?}"
            );
        }
    }
    assert!(unlinked_sim::compile(
        &source_model("RandomNumber", &[("Variance", "-1")]),
        &options(Solver::Rk4)
    )
    .is_err());
}

#[test]
fn zero_order_hold_requires_an_explicit_sample_period() {
    let mut model = source_model("Clock", &[]);
    let mut hold = source_model("ZeroOrderHold", &[]).root.blocks.remove(0);
    hold.id = "hold".into();
    model.root.blocks.push(hold);
    model.root.lines.push(unlinked_model::Line {
        src: Some(unlinked_model::Endpoint {
            block: "noise".into(),
            port: unlinked_model::PortRef {
                kind: unlinked_model::PortKind::Out,
                index: 1,
            },
        }),
        dst: Some(unlinked_model::Endpoint {
            block: "hold".into(),
            port: unlinked_model::PortRef {
                kind: unlinked_model::PortKind::In,
                index: 1,
            },
        }),
        ..Default::default()
    });
    for sample in [None, Some("-1"), Some("0")] {
        model.root.blocks[1].parameters.remove("SampleTime");
        if let Some(value) = sample {
            model.root.blocks[1]
                .parameters
                .insert("SampleTime".into(), value.into());
        }
        assert!(unlinked_sim::compile(&model, &options(Solver::Rk4)).is_err());
    }
    model.root.blocks[1]
        .parameters
        .insert("SampleTime".into(), "1".into());
    let trace = unlinked_sim::simulate_model(&model, &options(Solver::Rk4)).unwrap();
    assert_eq!(
        trace.signals["hold"],
        vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]
    );
}
