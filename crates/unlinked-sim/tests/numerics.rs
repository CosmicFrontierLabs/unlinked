use unlinked_sim::*;
fn node(id: &str, kind: Kind) -> Node {
    Node {
        id: id.into(),
        name: id.into(),
        kind,
    }
}
fn wire(source: &str, target: &str, input: usize) -> Wire {
    Wire {
        source: source.into(),
        target: target.into(),
        input,
    }
}
fn options() -> Options {
    Options {
        stop: 1.0,
        step: 0.01,
        ..Options::default()
    }
}
#[test]
fn scalar_chain_and_fanout() {
    let g = Graph {
        nodes: vec![
            node("c", Kind::Constant { value: 3.0 }),
            node("g", Kind::Gain { gain: 2.0 }),
            node(
                "s",
                Kind::Sum {
                    signs: vec![1.0, -1.0],
                },
            ),
        ],
        wires: vec![wire("c", "g", 0), wire("g", "s", 0), wire("c", "s", 1)],
    };
    let t = simulate(&g, &options()).unwrap();
    assert_eq!(t.time.len(), 101);
    assert!(t.signals["s"].iter().all(|x| *x == 3.0));
}
#[test]
fn exponential_feedback_rk4_and_euler() {
    let g = Graph {
        nodes: vec![
            node("x", Kind::Integrator { initial: 1.0 }),
            node("neg", Kind::Gain { gain: -1.0 }),
        ],
        wires: vec![wire("x", "neg", 0), wire("neg", "x", 0)],
    };
    let rk = simulate(&g, &options()).unwrap();
    assert!((rk.signals["x"][100] - (-1.0_f64).exp()).abs() < 1e-9);
    let eu = simulate(
        &g,
        &Options {
            solver: Solver::Euler,
            ..options()
        },
    )
    .unwrap();
    assert!((eu.signals["x"][100] - 0.99_f64.powi(100)).abs() < 1e-12);
}
#[test]
fn unit_delay_is_simultaneous() {
    let g = Graph {
        nodes: vec![
            node("a", Kind::UnitDelay { initial: 1.0 }),
            node("b", Kind::UnitDelay { initial: 2.0 }),
        ],
        wires: vec![wire("a", "b", 0), wire("b", "a", 0)],
    };
    let t = simulate(
        &g,
        &Options {
            stop: 2.0,
            step: 1.0,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(t.signals["a"], vec![1.0, 2.0, 1.0]);
    assert_eq!(t.signals["b"], vec![2.0, 1.0, 2.0]);
}
#[test]
fn step_boundary_and_partial_final_step() {
    let g = Graph {
        nodes: vec![
            node(
                "step",
                Kind::Step {
                    time: 0.5,
                    before: 0.0,
                    after: 1.0,
                },
            ),
            node("int", Kind::Integrator { initial: 0.0 }),
        ],
        wires: vec![wire("step", "int", 0)],
    };
    let t = simulate(
        &g,
        &Options {
            stop: 1.05,
            step: 0.1,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(*t.time.last().unwrap(), 1.05);
    assert!((t.signals["int"].last().unwrap() - 0.55).abs() < 1e-10);
}
#[test]
fn rejects_loops_bad_wires_and_budgets() {
    let mut g = Graph {
        nodes: vec![node("g", Kind::Gain { gain: 1.0 })],
        wires: vec![wire("g", "g", 0)],
    };
    assert!(matches!(
        simulate(&g, &options()),
        Err(Error::AlgebraicLoop(_))
    ));
    g.wires[0].source = "missing".into();
    assert!(matches!(
        simulate(&g, &options()),
        Err(Error::Connection(_))
    ));
    assert!(simulate(
        &Graph::default(),
        &Options {
            step: 1e-15,
            ..options()
        }
    )
    .is_err());
    assert!(simulate(
        &Graph::default(),
        &Options {
            step: f64::NAN,
            ..options()
        }
    )
    .is_err());
}
#[test]
fn divide_by_zero_is_diagnostic() {
    let g = Graph {
        nodes: vec![
            node("c", Kind::Constant { value: 0.0 }),
            node("p", Kind::Product { divide: vec![true] }),
        ],
        wires: vec![wire("c", "p", 0)],
    };
    assert!(matches!(simulate(&g, &options()), Err(Error::Block { .. })));
}

#[test]
fn decimal_step_boundary_does_not_integrate_future_value() {
    let g = Graph {
        nodes: vec![
            node(
                "step",
                Kind::Step {
                    time: 0.3,
                    before: 0.0,
                    after: 1.0,
                },
            ),
            node("int", Kind::Integrator { initial: 0.0 }),
        ],
        wires: vec![wire("step", "int", 0)],
    };
    let t = simulate(
        &g,
        &Options {
            stop: 1.0,
            step: 0.1,
            ..options()
        },
    )
    .unwrap();
    assert!((t.signals["int"].last().unwrap() - 0.7).abs() < 1e-12);
    assert_eq!(t.signals["int"][3], 0.0);
}

#[test]
fn accepted_transition_tolerance_is_consistent_across_stages() {
    let g = Graph {
        nodes: vec![
            node(
                "step",
                Kind::Step {
                    time: 0.30000000005,
                    before: 0.0,
                    after: 1.0,
                },
            ),
            node("int", Kind::Integrator { initial: 0.0 }),
        ],
        wires: vec![wire("step", "int", 0)],
    };
    let t = simulate(
        &g,
        &Options {
            stop: 1.0,
            step: 0.1,
            ..options()
        },
    )
    .unwrap();
    assert!((t.signals["int"].last().unwrap() - 0.7).abs() < 1e-12);
    assert_eq!(t.signals["step"][3], 1.0);
}
#[test]
fn discrete_delays_reject_off_grid_final_sample() {
    let g = Graph {
        nodes: vec![node("x", Kind::UnitDelay { initial: 1.0 })],
        wires: vec![wire("x", "x", 0)],
    };
    assert!(simulate(
        &g,
        &Options {
            stop: 1.05,
            step: 0.1,
            ..options()
        }
    )
    .unwrap_err()
    .to_string()
    .contains("stop time"));
}

#[test]
fn decimal_stop_time_does_not_add_duplicate_sample() {
    let t = simulate(
        &Graph::default(),
        &Options {
            stop: 0.14,
            step: 0.01,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(t.time.len(), 15);
    assert_eq!(*t.time.last().unwrap(), 0.14);
    assert!(t.time.windows(2).all(|x| x[0] < x[1]));
}

#[test]
fn final_off_grid_step_is_not_shifted() {
    for stop in [0.24, 0.25] {
        let g = Graph {
            nodes: vec![
                node(
                    "step",
                    Kind::Step {
                        time: stop,
                        before: 0.0,
                        after: 1.0,
                    },
                ),
                node("int", Kind::Integrator { initial: 0.0 }),
            ],
            wires: vec![wire("step", "int", 0)],
        };
        let t = simulate(
            &g,
            &Options {
                stop,
                step: 0.1,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(*t.signals["int"].last().unwrap(), 0.0);
        assert_eq!(*t.signals["step"].last().unwrap(), 1.0);
    }
}
#[test]
fn positive_tiny_duration_keeps_both_endpoints() {
    let t = simulate(
        &Graph::default(),
        &Options {
            stop: 1e-20,
            step: 1.0,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(t.time, vec![0.0, 1e-20]);
}
