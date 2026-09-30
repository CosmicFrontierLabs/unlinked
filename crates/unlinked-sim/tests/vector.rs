use std::collections::{BTreeMap, BTreeSet};
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
        charts: Vec::new(),
        name: "vectors".into(),
        source: SourceFormat::Mdl,
        charts: vec![],
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
        stop: 0.0,
        ..Options::default()
    }
}
#[test]
fn mux_and_demux_route_distinct_output_ports() {
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "1")]),
            block("b", "Constant", &[("Value", "2")]),
            block("mux", "Mux", &[("Inputs", "2")]),
            block("demux", "Demux", &[("Outputs", "2")]),
            block("sink", "Outport", &[]),
        ],
        vec![
            line("a", 1, "mux", 1),
            line("b", 1, "mux", 2),
            line("mux", 1, "demux", 1),
            line("demux", 2, "sink", 1),
        ],
    );
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["mux[1]"], vec![1.0]);
    assert_eq!(t.signals["mux[2]"], vec![2.0]);
    assert_eq!(t.signals["demux:out:1"], vec![1.0]);
    assert_eq!(t.signals["demux:out:2"], vec![2.0]);
    assert_eq!(t.signals["sink"], vec![2.0]);
}
#[test]
fn explicit_mux_demux_widths_preserve_vector_segments() {
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2]")]),
            block("b", "Constant", &[("Value", "[3 4 5 6]")]),
            block("mux", "Mux", &[("Inputs", "[2 4]")]),
            block("demux", "Demux", &[("Outputs", "[2 4]")]),
        ],
        vec![
            line("a", 1, "mux", 1),
            line("b", 1, "mux", 2),
            line("mux", 1, "demux", 1),
        ],
    );
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["demux:out:1[2]"], vec![2.0]);
    assert_eq!(t.signals["demux:out:2[4]"], vec![6.0]);
}
#[test]
fn elementwise_arithmetic_broadcasts_scalars() {
    let mut m = model(
        vec![
            block("a", "Constant", &[("Value", "3")]),
            block("gain", "Gain", &[("Gain", "K")]),
            block("b", "Constant", &[("Value", "5")]),
            block("sum", "Sum", &[("Inputs", "++")]),
            block("c", "Constant", &[("Value", "[2 4]")]),
            block("product", "Product", &[("Inputs", "2")]),
        ],
        vec![
            line("a", 1, "gain", 1),
            line("gain", 1, "sum", 1),
            line("b", 1, "sum", 2),
            line("sum", 1, "product", 1),
            line("c", 1, "product", 2),
        ],
    );
    m.workspace.insert("K".into(), "[10 20]".into());
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["product[1]"], vec![70.0]);
    assert_eq!(t.signals["product[2]"], vec![260.0]);
}
#[test]
fn matrix_gain_uses_column_major_coefficients_and_correct_contraction() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[1 2]")]),
            block(
                "g",
                "Gain",
                &[("Gain", "[1 3;2 4]"), ("Multiplication", "Matrix(K*u)")],
            ),
        ],
        vec![line("u", 1, "g", 1)],
    );
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["g[1]"], vec![7.0]);
    assert_eq!(t.signals["g[2]"], vec![10.0]);
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[1 2;3 4]")]),
            block(
                "g",
                "Gain",
                &[("Gain", "[2 0;0 3]"), ("Multiplication", "Matrix(K*u)")],
            ),
        ],
        vec![line("u", 1, "g", 1)],
    );
    let t = simulate_model(&m, &options()).unwrap();
    for (key, value) in [
        ("g[1,1]", 2.0),
        ("g[2,1]", 9.0),
        ("g[1,2]", 4.0),
        ("g[2,2]", 12.0),
    ] {
        assert_eq!(t.signals[key], vec![value]);
    }
}
#[test]
fn vector_feedback_integrates_independent_rates_and_matrix_oscillator() {
    let m = model(
        vec![
            block("x", "Integrator", &[("InitialCondition", "[1 2]")]),
            block("g", "Gain", &[("Gain", "[-1 -2]")]),
        ],
        vec![line("x", 1, "g", 1), line("g", 1, "x", 1)],
    );
    let o = Options {
        stop: 1.0,
        step: 0.3,
        solver: Solver::Rk45,
        relative_tolerance: 1e-10,
        absolute_tolerance: 1e-12,
        ..Options::default()
    };
    let t = simulate_model(&m, &o).unwrap();
    for (i, &time) in t.time.iter().enumerate() {
        assert!((t.signals["x[1]"][i] - (-time).exp()).abs() < 1e-9);
        assert!((t.signals["x[2]"][i] - 2.0 * (-2.0 * time).exp()).abs() < 1e-9);
    }
    let m = model(
        vec![
            block(
                "g",
                "Gain",
                &[("Gain", "[0 1;-1 0]"), ("Multiplication", "Matrix(K*u)")],
            ),
            block("x", "Integrator", &[("InitialCondition", "[1 0]")]),
        ],
        vec![line("x", 1, "g", 1), line("g", 1, "x", 1)],
    );
    let t = simulate_model(&m, &o).unwrap();
    for (i, &time) in t.time.iter().enumerate() {
        assert!((t.signals["x[1]"][i] - time.cos()).abs() < 1e-9);
        assert!((t.signals["x[2]"][i] + time.sin()).abs() < 1e-9);
    }
}
#[test]
fn scalar_initial_conditions_broadcast_and_delays_tick_once() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[1 2]")]),
            block("d", "UnitDelay", &[("InitialCondition", "0")]),
        ],
        vec![line("u", 1, "d", 1)],
    );
    let t = simulate_model(
        &m,
        &Options {
            stop: 2.0,
            step: 1.0,
            solver: Solver::Rk45,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(t.signals["d[1]"], vec![0.0, 1.0, 1.0]);
    assert_eq!(t.signals["d[2]"], vec![0.0, 2.0, 2.0]);
    let m = model(
        vec![
            block("x", "Integrator", &[("InitialCondition", "0")]),
            block(
                "g",
                "Gain",
                &[("Gain", "[0 1;-1 0]"), ("Multiplication", "Matrix(K*u)")],
            ),
        ],
        vec![line("x", 1, "g", 1), line("g", 1, "x", 1)],
    );
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["x[1]"], vec![0.0]);
    assert_eq!(t.signals["x[2]"], vec![0.0]);
}
#[test]
fn one_input_sum_product_and_logic_reduce_vectors() {
    for (kind, params, expected) in [
        ("Sum", vec![("Inputs", "-")], -3.0),
        ("Product", vec![("Inputs", "/")], 0.5),
        ("Logic", vec![("Inputs", "1"), ("Operator", "AND")], 1.0),
    ] {
        let m = model(
            vec![
                block("u", "Constant", &[("Value", "[1 2]")]),
                block("r", kind, &params),
            ],
            vec![line("u", 1, "r", 1)],
        );
        let t = simulate_model(&m, &options()).unwrap();
        assert_eq!(t.signals["r"], vec![expected]);
    }
}
#[test]
fn incompatible_shapes_unresolved_cycles_and_resource_excess_reject() {
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2]")]),
            block("b", "Constant", &[("Value", "[1 2 3]")]),
            block("s", "Sum", &[("Inputs", "++")]),
        ],
        vec![line("a", 1, "s", 1), line("b", 1, "s", 2)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("incompatible signal shapes"));
    let m = model(
        vec![block("mux", "Mux", &[("Inputs", "1")])],
        vec![line("mux", 1, "mux", 1)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("unresolved signal shape"));
    let m = model(
        vec![block("a", "Constant", &[("Value", "ones(1,1025)")])],
        vec![],
    );
    assert!(compile(&m, &options()).is_err());
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2 3]")]),
            block("d", "Demux", &[("Outputs", "2")]),
        ],
        vec![line("a", 1, "d", 1)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("divisible"));
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2]")]),
            block(
                "g",
                "Gain",
                &[("Gain", "[1 2 3]"), ("Multiplication", "Matrix(K*u)")],
            ),
        ],
        vec![line("a", 1, "g", 1)],
    );
    assert!(compile(&m, &options()).is_err());
}
#[test]
fn matrix_signals_are_not_silently_flattened_by_mux() {
    let m = model(
        vec![
            block(
                "a",
                "Constant",
                &[("Value", "[1 2]"), ("VectorParams1D", "off")],
            ),
            block("mux", "Mux", &[("Inputs", "1")]),
        ],
        vec![line("a", 1, "mux", 1)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("one-dimensional"));
}
#[test]
fn original_scalar_ids_survive_generated_name_collisions() {
    let m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2]")]),
            block("a[1]", "Constant", &[("Value", "9")]),
        ],
        vec![],
    );
    let graph = compile(&m, &options()).unwrap();
    let ids: BTreeSet<_> = graph.nodes.iter().map(|n| &n.id).collect();
    assert_eq!(ids.len(), graph.nodes.len());
    let t = simulate_model(&m, &options()).unwrap();
    assert_eq!(t.signals["a[1]"], vec![9.0]);
    assert_eq!(t.signals["a[1]#1"], vec![1.0]);
}

#[test]
fn aggregate_ports_and_output_elements_are_bounded_before_expansion() {
    let many_demux = (0..100)
        .map(|i| block(&format!("d{i}"), "Demux", &[("Outputs", "1024")]))
        .collect();
    assert!(compile(&model(many_demux, vec![]), &options())
        .unwrap_err()
        .to_string()
        .contains("port/parameter budget"));
    let many_sources = (0..100)
        .map(|i| block(&format!("s{i}"), "Constant", &[("Value", "ones(1,1024)")]))
        .collect();
    assert!(compile(&model(many_sources, vec![]), &options())
        .unwrap_err()
        .to_string()
        .contains("output element budget"));
}
#[test]
fn state_dimensions_and_unsupported_library_semantics_still_reject() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[1 2 3]")]),
            block("x", "Integrator", &[("InitialCondition", "[0 0]")]),
        ],
        vec![line("u", 1, "x", 1)],
    );
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("incompatible signal shapes"));
    let mut m = model(vec![block("u", "Constant", &[("Value", "[1 2]")])], vec![]);
    m.root.blocks[0].library_source = Some("custom/source".into());
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("library links"));
}
#[test]
fn vector_relational_results_control_elementwise_nonzero_switches() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[-1 2]")]),
            block("zero", "Constant", &[("Value", "0")]),
            block("r", "RelationalOperator", &[("Operator", ">")]),
            block("a", "Constant", &[("Value", "5")]),
            block("b", "Constant", &[("Value", "9")]),
            block("switch", "Switch", &[("Criteria", "u2 ~= 0")]),
        ],
        vec![
            line("u", 1, "r", 1),
            line("zero", 1, "r", 2),
            line("a", 1, "switch", 1),
            line("r", 1, "switch", 2),
            line("b", 1, "switch", 3),
        ],
    );
    let trace = simulate_model(&m, &options()).unwrap();
    assert_eq!(trace.signals["switch[1]"], vec![9.0]);
    assert_eq!(trace.signals["switch[2]"], vec![5.0]);
}

#[test]
fn vector_initial_condition_broadcasts_scalar_derivative_in_any_order() {
    let blocks = [
        block("c", "Constant", &[("Value", "1")]),
        block("s", "Sum", &[("Inputs", "n")]),
        block("i", "Integrator", &[("InitialCondition", "[1 2]")]),
        block("g", "Gain", &[("Gain", "-1")]),
    ];
    for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
        let mut m = model(
            order.iter().map(|&i| blocks[i].clone()).collect(),
            vec![
                line("c", 1, "s", 1),
                line("g", 1, "s", 2),
                line("s", 1, "i", 1),
                line("i", 1, "g", 1),
            ],
        );
        m.workspace.insert("n".into(), "2".into());
        let t = simulate_model(
            &m,
            &Options {
                stop: 1.0,
                step: 0.1,
                solver: Solver::Rk45,
                ..Options::default()
            },
        )
        .unwrap();
        assert!((t.signals["i[1]"][10] - 1.0).abs() < 1e-6);
        assert!((t.signals["i[2]"][10] - (1.0 + (-1.0f64).exp())).abs() < 1e-6);
    }
    let m = model(
        vec![
            block("c", "Constant", &[("Value", "2")]),
            block("i", "Integrator", &[("InitialCondition", "[1 3]")]),
        ],
        vec![line("c", 1, "i", 1)],
    );
    let t = simulate_model(
        &m,
        &Options {
            stop: 1.0,
            step: 1.0,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(t.signals["i[1]"], vec![1.0, 3.0]);
    assert_eq!(t.signals["i[2]"], vec![3.0, 5.0]);
}

#[test]
fn reverse_order_long_chain_uses_worklist() {
    let mut blocks = vec![block("c", "Constant", &[("Value", "2")])];
    let mut lines = Vec::new();
    let mut previous = "c".to_string();
    for i in 0..2500 {
        let id = format!("a{i}");
        blocks.push(block(&id, "Abs", &[]));
        lines.push(line(&previous, 1, &id, 1));
        previous = id;
    }
    blocks.reverse();
    let graph = compile(&model(blocks, lines), &options()).unwrap();
    assert_eq!(graph.nodes.len(), 2501);
}

#[test]
fn explicit_root_inputs_validate_bindings_and_use_workspace() {
    use unlinked_sim::{compile_with_inputs, evaluate_inputs, simulate_model_with_inputs};
    let mut m = model(
        vec![
            block("in", "Inport", &[("PortDimensions", "2")]),
            block("g", "Gain", &[("Gain", "2")]),
        ],
        vec![line("in", 1, "g", 1)],
    );
    m.workspace.insert("k".into(), "[3 4]".into());
    assert!(compile(&m, &options()).is_err());
    let inputs = evaluate_inputs(&m, &BTreeMap::from([("in".into(), "k + 1".into())])).unwrap();
    let t = simulate_model_with_inputs(&m, &options(), &inputs).unwrap();
    assert_eq!(t.signals["g[1]"], vec![8.0]);
    assert_eq!(t.signals["g[2]"], vec![10.0]);
    assert!(evaluate_inputs(&m, &BTreeMap::from([("g".into(), "2".into())])).is_err());
    assert!(evaluate_inputs(&m, &BTreeMap::from([("missing".into(), "2".into())])).is_err());
    assert!(compile_with_inputs(&m, &options(), &BTreeMap::new()).is_err());
    let scalar = evaluate_inputs(&m, &BTreeMap::from([("in".into(), "1".into())])).unwrap();
    assert!(compile_with_inputs(&m, &options(), &scalar).is_err());
    assert!(evaluate_inputs(&m, &BTreeMap::from([("in".into(), "NaN".into())])).is_err());
    assert!(evaluate_inputs(&m, &BTreeMap::from([("in".into(), "'ab'".into())])).is_err());
    m.root.blocks[0]
        .parameters
        .insert("OutDataTypeStr".into(), "uint16".into());
    assert!(compile_with_inputs(&m, &options(), &inputs).is_err());
}

#[test]
fn scalarization_rejects_parameter_text_amplification() {
    let mut b = block("c", "Constant", &[("Value", "ones(1,1024)")]);
    b.parameters
        .insert("Description".into(), "x".repeat(70_000));
    let error = compile(&model(vec![b], vec![]), &options()).unwrap_err();
    assert!(error.to_string().contains("text budget"));
}
