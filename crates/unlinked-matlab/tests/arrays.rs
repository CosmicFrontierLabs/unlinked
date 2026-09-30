use std::{collections::BTreeMap, path::PathBuf, process::Command};
use unlinked_matlab::{
    array_runtime::{self as rt, Index, Value, ValueKind},
    transpile, transpile_arrays,
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
fn compile_run(source: &str, label: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("unlinked-array-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("program.rs");
    std::fs::write(&path, source).unwrap();
    let executable = dir.join("program");
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "--emit=link,llvm-ir"])
        .arg(&path)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "generated Rust did not compile:\n{}\nsource: {}",
        String::from_utf8_lossy(&compiled.stderr),
        path.display()
    );
    assert!(dir.join("program.ll").exists(), "array LLVM output missing");
    let output = Command::new(&executable).output().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    output
}
#[test]
fn array_scripts_generate_native_control_flow_and_multiple_outputs() {
    let source = r#"
A = [1 2; 3 4];
B = A * [2; 1];
[r,c] = size(A);
fprintf('%g %g %g %g\n', r, c, B);
A(:,2) = [8;9];
disp(A(end));
x = [3 1 2];
x([1 2]) = x([2 1]);
disp(x);
i = 7;
for i = 1:0
end
assert(isempty(i));
count = 0;
while count < 5
 count = count + 1;
 if count == 2
  continue;
 end
 if count == 4
  break;
 end
end
disp(count);
[u,v] = pair(3);
fprintf('%g %g\n',u,v);
function [u,v] = pair(x)
 u=x;
 v=x^2;
end
"#;
    let generated = transpile(source).unwrap();
    assert!(generated.contains("while ("));
    let run = compile_run(&generated, "control");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        "2 2 4 10\n9\n1 3 2\n4\n3 9\n"
    );
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
    assert!(transpile_arrays(&nested, false).is_err());
    let run = compile_run(&transpile("A=[1 2];disp(A(0));").unwrap(), "bounds");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("indices must be positive"));
    let run = compile_run(&transpile_arrays("while 1\nend\n", false).unwrap(), "fuel");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("one million"));
}
fn parse_results(stdout: &[u8]) -> BTreeMap<String, (usize, usize, Vec<f64>)> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("RESULT:"))
        .map(|line| {
            let mut parts = line.splitn(4, ':');
            let name = parts.next().unwrap().into();
            let rows = parts.next().unwrap().parse().unwrap();
            let cols = parts.next().unwrap().parse().unwrap();
            let data = parts
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            (name, (rows, cols, data))
        })
        .collect()
}
#[test]
fn optional_corpus_functions_match_octave() {
    let Some(root) = std::env::var_os("UNLINKED_TEST_CASES") else {
        eprintln!("UNLINKED_TEST_CASES unset; array corpus differential skipped");
        return;
    };
    if Command::new("octave").arg("--version").output().is_err() {
        eprintln!("Octave unavailable; array corpus differential skipped");
        return;
    }
    let root = PathBuf::from(root);
    let files = [
        "uuv/functions/skew.m",
        "uuv/functions/rotation.m",
        "matlab/algorithms/Searching/binary_search.m",
        "matlab/algorithms/Searching/linear_search.m",
        "matlab/algorithms/Strings/isPalindrome.m",
        "matlab/algorithms/sorting/bubble_sort.m",
        "matlab/algorithms/maths/euclidean_distance.m",
        "matlab/algorithms/maths/find_factorial.m",
        "matlab/algorithms/maths/fibonacci_sequence.m",
    ];
    let mut source = String::new();
    let mut definitions = Vec::new();
    for file in files {
        let path = root.join("fixtures").join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("configured corpus file {} missing: {e}", path.display()));
        source.push_str(&text);
        source.push('\n');
        definitions.push(text);
    }
    let cases = [
        (
            "skew",
            "skew([1;2;3])",
            "f_skew(vec![Value::new(3,1,vec![1.,2.,3.]).unwrap()])",
        ),
        (
            "rotation",
            "rotation([0.1;0.2;0.3],[1;2;3])",
            "f_rotation(vec![Value::new(3,1,vec![0.1,0.2,0.3]).unwrap(),Value::new(3,1,vec![1.,2.,3.]).unwrap()])",
        ),
        (
            "binary",
            "binary_search([1 3 5 7 9],7)",
            "f_binary_search(vec![Value::row(&[1.,3.,5.,7.,9.]).unwrap(),Value::scalar(7.)])",
        ),
        (
            "linear_yes",
            "linear_search([1 3 5],3)",
            "f_linear_search(vec![Value::row(&[1.,3.,5.]).unwrap(),Value::scalar(3.)])",
        ),
        (
            "linear_no",
            "linear_search([1 3 5],9)",
            "f_linear_search(vec![Value::row(&[1.,3.,5.]).unwrap(),Value::scalar(9.)])",
        ),
        (
            "palindrome_yes",
            "isPalindrome('racecar')",
            "f_isPalindrome(vec![Value::string(\"racecar\").unwrap()])",
        ),
        (
            "palindrome_no",
            "isPalindrome('rust')",
            "f_isPalindrome(vec![Value::string(\"rust\").unwrap()])",
        ),
        (
            "sort",
            "bubble_sort([4 1 3 2 -1])",
            "f_bubble_sort(vec![Value::row(&[4.,1.,3.,2.,-1.]).unwrap()])",
        ),
        (
            "distance",
            "euclidean_distance([1 2 3],[4 6 3])",
            "f_euclidean_distance(vec![Value::row(&[1.,2.,3.]).unwrap(),Value::row(&[4.,6.,3.]).unwrap()])",
        ),
        (
            "factorial",
            "find_factorial(6)",
            "f_find_factorial(vec![Value::scalar(6.)])",
        ),
        ("fibonacci", "fibo(10)", "f_fibo(vec![Value::scalar(10.)])"),
    ];
    let mut generated = transpile_arrays(&source, true).unwrap();
    generated.push_str("\nfn main(){\n");
    for (name, _, rust) in &cases {
        generated.push_str(&format!("{{let mut values={rust}.unwrap();let value=values.remove(0);print!(\"RESULT:{name}:{{}}:{{}}:\",value.rows,value.cols);for x in value.data{{print!(\"{{:.17}} \",x);}}println!();}}\n"));
    }
    generated.push_str("}\n");
    let actual = compile_run(&generated, "corpus");
    assert!(
        actual.status.success(),
        "{}",
        String::from_utf8_lossy(&actual.stderr)
    );
    let actual = parse_results(&actual.stdout);
    // Put fixed, licensed corpus functions in an Octave script with a leading
    // statement so Octave treats all subsequent functions as local definitions.
    let mut reference = String::from("1;\n");
    for definition in definitions {
        reference.push_str(&definition);
        reference.push('\n');
    }
    for (name, octave, _) in &cases {
        reference.push_str(&format!("result={octave};fprintf('RESULT:{name}:%d:%d:',rows(result),columns(result));fprintf('%.17g ',result(:));fprintf('\\n');\n"));
    }
    let dir = std::env::temp_dir().join(format!("unlinked-array-octave-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("reference.m");
    std::fs::write(&script, reference).unwrap();
    let expected = Command::new("octave")
        .args(["--no-gui", "--quiet"])
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        expected.status.success(),
        "{}",
        String::from_utf8_lossy(&expected.stderr)
    );
    let expected = parse_results(&expected.stdout);
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(actual.len(), cases.len());
    assert_eq!(expected.len(), cases.len());
    for (name, (rows, cols, data)) in expected {
        let value = actual.get(&name).unwrap();
        assert_eq!((value.0, value.1), (rows, cols), "{name} shape");
        assert_eq!(value.2.len(), data.len());
        for (a, b) in value.2.iter().zip(data) {
            assert!(
                (a - b).abs() < 1e-10_f64.max(b.abs() * 1e-12),
                "{name}: {a} != {b}"
            );
        }
    }
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
            .contains("unlinked_array_runtime")
    );
    assert!(
        unlinked_matlab::transpile_library("function y=f(x)\ny=x(1);\nend")
            .unwrap()
            .contains("args:Vec<Value>")
    );
    assert!(
        !unlinked_matlab::transpile_library("function y=f(x)\ny=x^2;\nend")
            .unwrap()
            .contains("unlinked_array_runtime")
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
fn octave_differential_array_expressions() {
    if Command::new("octave").arg("--version").output().is_err() {
        eprintln!("Octave unavailable; array expression differential skipped");
        return;
    }
    let expressions = [
        "A'",
        "A.'",
        "A(:)",
        "A([1;4])",
        "v([1;3])",
        "c([1 3])",
        "v([])",
        "A([],:)",
        "A(A>3)",
        "v(v>2)",
        "A(:,2:end)",
        "A(end:-1:1)",
        "v+c",
        "A.*[2;3]",
        "sum(A)",
        "sum(A,2)",
        "sum([])",
        "prod([])",
        "min([])",
        "max([])",
        "zeros(0,3)",
        "zeros(3,0)",
        "length(zeros(3,0))",
        "reshape(1:6,2,3)",
        "A*[1;2;3]",
        "[2 1;1 3]\\[5;7]",
        "[1 2]/[2 0;0 4]",
        "[2 1;1 3]^-1",
        "diag(v)",
        "diag(A)",
        "diag([])",
        "find(A>3)",
        "find(v>2)",
        "s(end:-1:1)",
        "num2str(1000000)",
        "linspace(3,5,0)",
        "1:0:3",
        "0:0.1:0.3",
        "[]+1",
        "ones(3,0)*ones(0,2)",
        "all([])",
        "any([])",
        "[zeros(0,3);zeros(0,3)]",
        "[zeros(3,0),zeros(3,0)]",
        "[zeros(0,3),zeros(0,4)]",
        "[zeros(2,0),ones(2,2)]",
        "[zeros(0,3);ones(2,3)]",
        "[[],zeros(0,3)]",
        "[zeros(0,3);[]]",
        "find([])",
        "find(0)",
        "find(false)",
        "find(zeros(0,3))",
        "find(zeros(3,0))",
        "sort([NaN -NaN -1 0 Inf -Inf])",
    ];
    let workspace = BTreeMap::from([
        ("A".into(), matrix(2, 3, &[1., 4., 2., 5., 3., 6.])),
        ("v".into(), Value::row(&[1., 2., 3.]).unwrap()),
        ("c".into(), matrix(3, 1, &[4., 5., 6.])),
        ("s".into(), Value::string("abc").unwrap()),
    ]);
    let initial = "A=[1 2 3;4 5 6];v=[1 2 3];c=[4;5;6];s='abc';\n";
    let mut reference = String::from(initial);
    let mut functions = String::new();
    for (i, expression) in expressions.iter().enumerate() {
        reference.push_str(&format!("result={expression};fprintf('RESULT:c{i}:%d:%d:',rows(result),columns(result));fprintf('%.17g ',double(result(:)));fprintf('\\n');\n"));
        functions.push_str(&format!(
            "function result=case{i}()\n{initial}result={expression};\nend\n"
        ));
    }
    let reference = Command::new("octave")
        .args(["--no-gui", "--quiet", "--eval", &reference])
        .output()
        .unwrap();
    assert!(
        reference.status.success(),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );
    let reference = parse_results(&reference.stdout);
    let mut generated = transpile_arrays(&functions, true).unwrap();
    generated.push_str("fn main(){");
    for i in 0..expressions.len() {
        generated.push_str(&format!("{{let value=f_case{i}(vec![]).unwrap().remove(0);print!(\"RESULT:c{i}:{{}}:{{}}:\",value.rows,value.cols);for x in value.data{{print!(\"{{:.17}} \",x);}}println!();}}"));
    }
    generated.push('}');
    let compiled = compile_run(&generated, "array-expressions");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let compiled = parse_results(&compiled.stdout);
    assert_eq!(reference.len(), expressions.len());
    assert_eq!(compiled.len(), expressions.len());
    for (i, expression) in expressions.iter().enumerate() {
        let expected = &reference[&format!("c{i}")];
        let generated = &compiled[&format!("c{i}")];
        let evaluated = unlinked_matlab::eval_array_expr(expression, &workspace)
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        for value in [evaluated, matrix(generated.0, generated.1, &generated.2)] {
            assert_eq!(
                (value.rows, value.cols),
                (expected.0, expected.1),
                "{expression}: shape"
            );
            assert_eq!(value.data.len(), expected.2.len());
            for (a, b) in value.data.iter().zip(&expected.2) {
                assert!(
                    (a - b).abs() < 1e-10 || a == b || a.is_nan() && b.is_nan(),
                    "{expression}: {a} != {b}"
                );
            }
        }
    }
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
                let _ = transpile_arrays(&source, false);
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
    assert!(transpile_arrays(&format!("A=[{deep}];"), false).is_err());
    assert!(unlinked_matlab::eval_array_expr("2^[1 2;3 4]", &BTreeMap::new()).is_err());
    assert!(unlinked_matlab::eval_array_expr("sqrt([-1 0])", &BTreeMap::new()).is_err());
}

#[test]
fn aggregate_parameter_budgets_and_generated_recursion_are_enforced() {
    let repeated = ["sum(ones(1000))"; 9].join("+");
    let error = unlinked_matlab::eval_array_expr(&repeated, &BTreeMap::new()).unwrap_err();
    assert!(error.message.contains("aggregate"));
    let error = unlinked_matlab::eval_array_expr("eye(100)^1024", &BTreeMap::new()).unwrap_err();
    assert!(error.message.contains("operation budget"));
    let source = "A=[1];disp(recur(A));\nfunction y=recur(x)\ny=recur(x);\nend\n";
    let run = compile_run(&transpile(source).unwrap(), "recursion");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("recursion exceeds 64"));
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
