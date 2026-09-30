use unlinked_matlab::ArrayBudget;
use unlinked_sim::{
    simulate, simulate_with_observer_and_budget, Graph, Kind, Node, Options, Solver, Wire,
};
fn graph(script: &str) -> Graph {
    Graph {
        nodes: vec![
            Node {
                id: "source".into(),
                name: "source".into(),
                kind: Kind::Clock,
            },
            Node {
                id: "f".into(),
                name: "f".into(),
                kind: Kind::MatlabFunction {
                    script: script.into(),
                    inputs: 1,
                },
            },
        ],
        wires: vec![Wire {
            source: "source".into(),
            target: "f".into(),
            input: 0,
        }],
    }
}
#[test]
fn function_evaluates_scalar_inputs_at_every_continuous_solver_stage() {
    let mut graph = graph("function y=f(u)\ny=3*u^2;");
    graph.nodes.push(Node {
        id: "integrator".into(),
        name: "integrator".into(),
        kind: Kind::Integrator { initial: 0. },
    });
    graph.wires.push(Wire {
        source: "f".into(),
        target: "integrator".into(),
        input: 0,
    });
    for solver in [Solver::Rk4, Solver::Rk45] {
        let trace = simulate(
            &graph,
            &Options {
                stop: 1.,
                step: 0.05,
                solver,
                ..Options::default()
            },
        )
        .unwrap();
        for (i, &t) in trace.time.iter().enumerate() {
            assert!((trace.signals["f"][i] - 3. * t * t).abs() < 1e-12);
            assert!((trace.signals["integrator"][i] - t * t * t).abs() < 1e-10);
        }
    }
}
#[test]
fn wrong_arity_nonfinite_arrays_state_and_io_reject() {
    for script in [
        "function y=f(a,b)\ny=a+b;\nend",
        "function [a,b]=f(u)\na=u;b=u;\nend",
        "function y=f(u)\ny=[u u];\nend",
        "function y=f(u)\ny=Inf;\nend",
        "function y=f(u)\ny='a';\nend",
        "function y=f(u)\npersistent x;\ny=u;\nend",
        "function y=f(u)\ny=u;\nif false\ndisp(u);\nend\nend",
    ] {
        assert!(
            simulate(
                &graph(script),
                &Options {
                    stop: 0.,
                    ..Options::default()
                }
            )
            .is_err(),
            "{script}"
        );
    }
    let mut graph = graph("function y=f(u)\ny=u;\nend");
    if let Kind::MatlabFunction { inputs, .. } = &mut graph.nodes[1].kind {
        *inputs = usize::MAX;
    }
    assert!(simulate(
        &graph,
        &Options {
            stop: 0.,
            ..Options::default()
        }
    )
    .is_err());
}
#[test]
fn function_work_budget_is_shared_across_nodes_and_samples_and_interruptible() {
    let graph = graph("function y=f(u)\ny=u+1;\nend");
    let options = Options {
        stop: 10.,
        step: 1.,
        solver: Solver::Euler,
        ..Options::default()
    };
    let mut samples = 0;
    let error = simulate_with_observer_and_budget(
        &graph,
        &options,
        ArrayBudget::with_limits(1000, 100),
        |_| {
            samples += 1;
            true
        },
    )
    .unwrap_err();
    assert!(
        samples > 0 && samples < 11,
        "budget was reset each sample: {samples}"
    );
    assert!(error.to_string().contains("budget"), "{error}");
    let error = simulate_with_observer_and_budget(
        &graph,
        &options,
        ArrayBudget::default().with_cancellation(|| true),
        |_| true,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("execution interrupted"),
        "{error}"
    );
}
#[test]
fn multiple_scalar_inputs_and_zero_input_constants_are_supported() {
    let mut graph = graph("function y=f(a,b)\ny=a+2*b;\nend");
    if let Kind::MatlabFunction { inputs, .. } = &mut graph.nodes[1].kind {
        *inputs = 2;
    }
    graph.wires.push(Wire {
        source: "source".into(),
        target: "f".into(),
        input: 1,
    });
    let trace = simulate(
        &graph,
        &Options {
            stop: 1.,
            step: 1.,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(trace.signals["f"], vec![0., 3.]);
    graph.nodes[1].kind = Kind::MatlabFunction {
        script: "function y=f()\ny=7;\nend".into(),
        inputs: 0,
    };
    graph.wires.clear();
    assert_eq!(
        simulate(
            &graph,
            &Options {
                stop: 0.,
                ..Options::default()
            }
        )
        .unwrap()
        .signals["f"],
        vec![7.]
    );
}
