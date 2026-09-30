use unlinked_sim::{simulate, Graph, Kind, Node, Options, Wire};
fn run(kind: Kind, inputs: &[f64]) -> f64 {
    let mut graph = Graph::default();
    for (index, value) in inputs.iter().enumerate() {
        let id = format!("input{index}");
        graph.nodes.push(Node {
            id: id.clone(),
            name: id.clone(),
            kind: Kind::Constant { value: *value },
        });
        graph.wires.push(Wire {
            source: id,
            target: "output".into(),
            input: index,
        });
    }
    graph.nodes.push(Node {
        id: "output".into(),
        name: "output".into(),
        kind,
    });
    simulate(
        &graph,
        &Options {
            stop: 0.0,
            ..Options::default()
        },
    )
    .unwrap()
    .signals["output"][0]
}
#[test]
fn boolean_truth_tables_include_numeric_nonzero_inputs() {
    for bits in 0u32..8 {
        let inputs: Vec<_> = (0..3)
            .map(|p| if bits & (1 << p) != 0 { -2.0 } else { 0.0 })
            .collect();
        let count = bits.count_ones();
        for (operation, expected) in [
            ("AND", count == 3),
            ("OR", count > 0),
            ("NAND", count != 3),
            ("NOR", count == 0),
            ("XOR", count % 2 == 1),
            ("NXOR", count % 2 == 0),
        ] {
            assert_eq!(
                run(
                    Kind::Logic {
                        operation: operation.into(),
                        inputs: 3
                    },
                    &inputs
                ),
                f64::from(expected)
            );
        }
    }
    assert_eq!(
        run(
            Kind::Logic {
                operation: "NOT".into(),
                inputs: 1
            },
            &[0.0]
        ),
        1.0
    );
    assert_eq!(
        run(
            Kind::Logic {
                operation: "NOT".into(),
                inputs: 1
            },
            &[-3.0]
        ),
        0.0
    );
}
#[test]
fn relational_comparisons_are_exact_scalar_comparisons() {
    for (operation, values) in [
        ("==", [false, true, false]),
        ("~=", [true, false, true]),
        ("<", [true, false, false]),
        ("<=", [true, true, false]),
        (">", [false, false, true]),
        (">=", [false, true, true]),
    ] {
        for (value, expected) in [-1.0, 0.0, 1.0].into_iter().zip(values) {
            assert_eq!(
                run(
                    Kind::Relational {
                        operation: operation.into()
                    },
                    &[value, 0.0]
                ),
                f64::from(expected)
            );
        }
    }
}
#[test]
fn switch_uses_second_input_as_nonzero_control() {
    assert_eq!(run(Kind::Switch, &[17.0, -2.0, 23.0]), 17.0);
    assert_eq!(run(Kind::Switch, &[17.0, -0.0, 23.0]), 23.0);
}
#[test]
fn invalid_logic_counts_fail_before_port_allocation() {
    for inputs in [0, 1025, usize::MAX] {
        let graph = Graph {
            nodes: vec![Node {
                id: "x".into(),
                name: "x".into(),
                kind: Kind::Logic {
                    operation: "OR".into(),
                    inputs,
                },
            }],
            wires: vec![],
        };
        assert!(simulate(&graph, &Options::default()).is_err());
    }
}
