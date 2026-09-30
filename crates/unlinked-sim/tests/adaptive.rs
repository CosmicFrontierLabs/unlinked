use unlinked_sim::{simulate, Error, Graph, Kind, Node, Options, Solver, Wire};
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
        step: 1.0,
        solver: Solver::Rk45,
        relative_tolerance: 1e-10,
        absolute_tolerance: 1e-12,
        ..Options::default()
    }
}
fn decay(rate: f64) -> Graph {
    Graph {
        nodes: vec![
            node("x", Kind::Integrator { initial: 1.0 }),
            node("rate", Kind::Gain { gain: -rate }),
        ],
        wires: vec![wire("x", "rate", 0), wire("rate", "x", 0)],
    }
}
#[test]
fn adaptive_substeps_resolve_fast_decay_on_coarse_output_grid() {
    let trace = simulate(&decay(10.0), &options()).unwrap();
    assert_eq!(trace.time, vec![0.0, 1.0]);
    assert!((trace.signals["x"][1] - (-10.0_f64).exp()).abs() < 1e-12);
}
#[test]
fn tighter_tolerances_improve_feedback_accuracy() {
    let graph = decay(2.0);
    let tight = simulate(&graph, &options()).unwrap();
    let loose = simulate(
        &graph,
        &Options {
            relative_tolerance: 1e-2,
            absolute_tolerance: 1e-4,
            ..options()
        },
    )
    .unwrap();
    let exact = (-2.0_f64).exp();
    assert!((tight.signals["x"][1] - exact).abs() < (loose.signals["x"][1] - exact).abs() / 100.0);
}
#[test]
fn coupled_oscillator_states_advance_simultaneously() {
    let graph = Graph {
        nodes: vec![
            node("position", Kind::Integrator { initial: 1.0 }),
            node("velocity", Kind::Integrator { initial: 0.0 }),
            node("acceleration", Kind::Gain { gain: -1.0 }),
        ],
        wires: vec![
            wire("velocity", "position", 0),
            wire("position", "acceleration", 0),
            wire("acceleration", "velocity", 0),
        ],
    };
    let trace = simulate(
        &graph,
        &Options {
            stop: 6.5,
            step: 2.0,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(trace.time, vec![0.0, 2.0, 4.0, 6.0, 6.5]);
    for (i, &t) in trace.time.iter().enumerate() {
        assert!((trace.signals["position"][i] - t.cos()).abs() < 2e-10);
        assert!((trace.signals["velocity"][i] + t.sin()).abs() < 2e-10);
    }
}
#[test]
fn step_events_use_left_limits_and_restart_on_right_side() {
    let graph = Graph {
        nodes: vec![
            node(
                "step",
                Kind::Step {
                    time: 0.3,
                    before: 0.0,
                    after: 2.0,
                },
            ),
            node("x", Kind::Integrator { initial: 1.0 }),
        ],
        wires: vec![wire("step", "x", 0)],
    };
    let trace = simulate(
        &graph,
        &Options {
            stop: 0.45,
            step: 0.1,
            ..options()
        },
    )
    .unwrap();
    for (i, &t) in trace.time.iter().enumerate() {
        assert!((trace.signals["x"][i] - (1.0 + 2.0 * (t - 0.3).max(0.0))).abs() < 1e-12);
    }
    assert_eq!(trace.signals["step"][3], 2.0);
}
#[test]
fn internal_substeps_do_not_tick_discrete_delays() {
    let mut graph = decay(10.0); // Forces rejected/internal steps.
    graph.nodes.extend([
        node("clock", Kind::Clock),
        node("delay", Kind::UnitDelay { initial: 0.0 }),
        node("integral", Kind::Integrator { initial: 0.0 }),
    ]);
    graph
        .wires
        .extend([wire("clock", "delay", 0), wire("delay", "integral", 0)]);
    let trace = simulate(
        &graph,
        &Options {
            stop: 3.0,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(trace.signals["delay"], vec![0.0, 0.0, 1.0, 2.0]);
    for (actual, expected) in trace.signals["integral"].iter().zip([0.0, 0.0, 0.0, 1.0]) {
        assert!((actual - expected).abs() < 1e-12);
    }
}
#[test]
fn rejected_steps_and_attempts_across_output_intervals_are_bounded() {
    let error = simulate(
        &decay(10.0),
        &Options {
            max_internal_steps: 1,
            ..options()
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::Options(s) if s.contains("step budget")));
    let error = simulate(
        &decay(0.0),
        &Options {
            stop: 3.0,
            max_internal_steps: 2,
            ..options()
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::Options(s) if s.contains("step budget")));
}
#[test]
fn validates_tolerances_and_restores_defaults_for_old_json() {
    let old: Options = serde_json::from_str(r#"{"stop":1,"step":0.1,"solver":"rk45"}"#).unwrap();
    assert_eq!(old.relative_tolerance, 1e-6);
    for invalid in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        assert!(simulate(
            &decay(1.0),
            &Options {
                relative_tolerance: invalid,
                ..options()
            }
        )
        .is_err());
        assert!(simulate(
            &decay(1.0),
            &Options {
                absolute_tolerance: invalid,
                ..options()
            }
        )
        .is_err());
    }
    assert!(simulate(
        &decay(1.0),
        &Options {
            relative_tolerance: 2.0,
            ..options()
        }
    )
    .is_err());
    assert!(simulate(
        &decay(1.0),
        &Options {
            max_internal_steps: 0,
            ..options()
        }
    )
    .is_err());
}
