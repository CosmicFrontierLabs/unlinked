use std::collections::BTreeMap;
use unlinked_matlab::{
    array_runtime::{self as rt, Index, Value, ValueKind},
    transpile, transpile_typed,
};
fn matrix(rows: usize, cols: usize, data: &[f64]) -> Value {
    Value::new(rows, cols, data.to_vec()).unwrap()
}
fn index(data: &[f64]) -> Index {
    Index::Values(Value::row(data).unwrap())
}
fn assert_matrix(value: &Value, rows: usize, cols: usize, expected: &[f64]) {
    assert_eq!((value.rows, value.cols), (rows, cols));
    assert_eq!(value.data.len(), expected.len());
    for (a, b) in value.data.iter().zip(expected) {
        assert!(
            (a - b).abs() < 1e-10 || a == b || a.is_nan() && b.is_nan(),
            "{a} != {b}"
        );
    }
}
#[test]
fn column_major_indexing_shapes_growth_and_logical_masks() {
    let mut a = matrix(2, 3, &[1., 4., 2., 5., 3., 6.]);
    assert_matrix(
        &a.index(&[index(&[6., 1., 3.])]).unwrap(),
        1,
        3,
        &[6., 1., 2.],
    );
    assert_matrix(
        &a.index(&[Index::All, index(&[2., 3.])]).unwrap(),
        2,
        2,
        &[2., 5., 3., 6.],
    );
    let mask = rt::binary(">", &a, &Value::scalar(3.)).unwrap();
    assert_eq!(mask.kind, ValueKind::Logical);
    assert_matrix(
        &a.index(&[Index::Values(mask)]).unwrap(),
        3,
        1,
        &[4., 5., 6.],
    );
    a.assign(&[index(&[1., 2.]), index(&[2.])], &Value::scalar(9.))
        .unwrap();
    assert_matrix(&a, 2, 3, &[1., 4., 9., 9., 3., 6.]);
    let mut row = Value::row(&[1., 2.]).unwrap();
    row.assign(&[index(&[4.])], &Value::scalar(7.)).unwrap();
    assert_matrix(&row, 1, 4, &[1., 2., 0., 7.]);
    assert!(a.index(&[index(&[0.])]).is_err());
    assert!(a.index(&[index(&[1.5])]).is_err());
    assert!(a.index(&[index(&[7.])]).is_err());
    assert!(a.assign(&[index(&[1.])], &Value::empty()).is_err());
}
#[test]
fn matrix_arithmetic_solve_broadcast_and_concatenation() {
    let a = matrix(2, 2, &[2., 1., 1., 3.]);
    let b = matrix(2, 1, &[5., 7.]);
    let solved = rt::binary("\\", &a, &b).unwrap();
    assert_matrix(&solved, 2, 1, &[1.6, 1.8]);
    assert_matrix(&rt::binary("*", &a, &solved).unwrap(), 2, 1, &[5., 7.]);
    assert_matrix(
        &rt::binary("^", &a, &Value::scalar(2.)).unwrap(),
        2,
        2,
        &[5., 5., 5., 10.],
    );
    assert_matrix(
        &rt::binary(
            "+",
            &Value::row(&[1., 2., 3.]).unwrap(),
            &matrix(2, 1, &[10., 20.]),
        )
        .unwrap(),
        2,
        3,
        &[11., 21., 12., 22., 13., 23.],
    );
    let joined = rt::concatenate(vec![
        vec![Value::scalar(1.), Value::scalar(2.)],
        vec![Value::scalar(3.), Value::scalar(4.)],
    ])
    .unwrap();
    assert_matrix(&joined, 2, 2, &[1., 3., 2., 4.]);
    assert!(rt::binary("\\", &matrix(2, 2, &[1., 2., 2., 4.]), &b).is_err());
    assert!(rt::builtin("zeros", vec![Value::scalar(100_001.)], 1).is_err());
}
#[test]
fn array_runtime_diagnostics_and_parser_limits() {
    for source in [
        "A=[1];system('echo unsafe');",
        "A=[1];fopen('file');",
        "A=[1];disp(missing);",
        "A=[1];break;",
        "A=[1];disp(end);",
        "A=[1,];",
        "A=[1];disp(A());",
    ] {
        assert!(transpile(source).is_err(), "accepted {source}");
    }
    let nested = format!("A=[{}1{}];", "(".repeat(80), ")".repeat(80));
    assert!(transpile_typed(&nested, false).is_err());
}
#[test]
fn pure_array_parameter_expressions_and_dispatch() {
    let workspace = BTreeMap::from([
        ("A".into(), matrix(2, 2, &[1., 3., 2., 4.])),
        ("gain".into(), Value::scalar(2.)),
    ]);
    assert_matrix(
        &unlinked_matlab::eval_array_expr("A(:,end)*gain", &workspace).unwrap(),
        2,
        1,
        &[4., 8.],
    );
    assert_matrix(
        &unlinked_matlab::eval_array_expr("[1 -2; 3 - 4 5]", &BTreeMap::new()).unwrap(),
        2,
        2,
        &[1., -1., -2., 5.],
    );
    for source in [
        "fprintf('unsafe')",
        "system('unsafe')",
        "disp(A)",
        "A(0)",
        "A(1)=2",
        "zeros(1e9)",
    ] {
        assert!(
            unlinked_matlab::eval_array_expr(source, &workspace).is_err(),
            "{source}"
        );
    }
    assert!(
        transpile("x=1:3;disp(x);")
            .unwrap()
            .contains("unlinked_matlab_rt")
    );
    assert!(
        unlinked_matlab::transpile_library("function y=f(x)\ny=x(1);\nend")
            .unwrap()
            .contains("ArrayD<f64>")
    );
    assert!(
        unlinked_matlab::transpile_library("function y=f(x)\ny=x^2;\nend")
            .unwrap()
            .contains("unlinked_matlab_rt")
    );
    let malformed = BTreeMap::from([(
        "bad".into(),
        Value {
            rows: 2,
            cols: 2,
            data: vec![],
            kind: ValueKind::Numeric,
        },
    )]);
    assert!(unlinked_matlab::eval_array_expr("bad(1)", &malformed).is_err());
}

#[test]
fn array_parser_malformed_input_and_deep_expression_are_bounded() {
    let alphabet = b"ABCxyz0123+-*/^()=:;,\n%[]'~&|\\.";
    let mut seed = 0x713a92_u64;
    for length in 0..128 {
        for _ in 0..20 {
            let source: String = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    alphabet[(seed >> 32) as usize % alphabet.len()] as char
                })
                .collect();
            let result = std::panic::catch_unwind(|| {
                let _ = transpile_typed(&source, false);
                let _ = unlinked_matlab::eval_array_expr(&source, &BTreeMap::new());
            });
            assert!(result.is_ok(), "array parser panic for {source:?}");
        }
    }
    let mut deep = String::from("1");
    for _ in 0..30 {
        deep = format!("({deep})+{}", vec!["1"; 20].join("+"));
    }
    assert!(unlinked_matlab::eval_array_expr(&deep, &BTreeMap::new()).is_err());
    assert!(transpile_typed(&format!("A=[{deep}];"), false).is_err());
    assert!(unlinked_matlab::eval_array_expr("2^[1 2;3 4]", &BTreeMap::new()).is_err());
    assert!(unlinked_matlab::eval_array_expr("sqrt([-1 0])", &BTreeMap::new()).is_err());
}

#[test]
fn aggregate_parameter_budgets_are_enforced() {
    let repeated = ["sum(ones(1000))"; 9].join("+");
    let error = unlinked_matlab::eval_array_expr(&repeated, &BTreeMap::new()).unwrap_err();
    assert!(error.message.contains("aggregate"));
    let error = unlinked_matlab::eval_array_expr("eye(100)^1024", &BTreeMap::new()).unwrap_err();
    assert!(error.message.contains("operation budget"));
}

#[test]
fn parameter_budget_can_be_shared_across_entire_model() {
    let mut budget = unlinked_matlab::ArrayBudget::with_limits(8, 1000);
    let ws = BTreeMap::new();
    unlinked_matlab::eval_array_expr_with_budget("ones(2)", &ws, &mut budget).unwrap();
    assert!(budget.remaining_elements() < 8);
    assert!(unlinked_matlab::eval_array_expr_with_budget("ones(2)", &ws, &mut budget).is_err());
    let mut budget = unlinked_matlab::ArrayBudget::with_limits(100, 1);
    assert!(unlinked_matlab::eval_array_expr_with_budget("missing+1", &ws, &mut budget).is_err());
}

#[test]
fn wide_matrix_literals_are_bounded_by_size_not_expression_tree_depth() {
    let row = ["0"; 64].join(" ");
    let literal = format!("[{}]", vec![row; 64].join(";"));
    let value = unlinked_matlab::eval_array_expr(&literal, &BTreeMap::new()).unwrap();
    assert_eq!((value.rows, value.cols, value.data.len()), (64, 64, 4096));
}

#[test]
fn shaped_empty_concatenation_rejects_dimension_mismatches() {
    for expression in [
        "[zeros(0,3);zeros(0,4)]",
        "[zeros(3,0),zeros(4,0)]",
        "[zeros(0,4);ones(2,3)]",
        "[zeros(3,0),ones(2,2)]",
    ] {
        assert!(
            unlinked_matlab::eval_array_expr(expression, &BTreeMap::new()).is_err(),
            "{expression}"
        );
    }
}
