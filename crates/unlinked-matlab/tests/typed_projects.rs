//! End-to-end checks compile only fixed repository-owned MATLAB fixtures.
//! Generated Cargo projects are built offline, never uploaded source code.
use std::{collections::BTreeMap, path::PathBuf, process::Command};
use unlinked_matlab::{
    array_runtime::Value, eval_array_expr, eval_script, project::generate_project,
};

struct Projects {
    root: PathBuf,
}
impl Projects {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("unlinked-typed-projects-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn run(&self, name: &str, source: &str, library: bool, caller: Option<&str>) -> String {
        self.run_inner(name, source, library, caller, true)
    }
    fn run_failure(&self, name: &str, source: &str) -> String {
        self.run_inner(name, source, false, None, false)
    }
    fn run_inner(
        &self,
        name: &str,
        source: &str,
        library: bool,
        caller: Option<&str>,
        success: bool,
    ) -> String {
        let project = generate_project(source, library).unwrap_or_else(|e| panic!("{name}: {e}"));
        for forbidden in [
            "Environment",
            "rt.tick",
            "Vec<Value>",
            "unlinked_array_runtime",
        ] {
            assert!(
                !project.source.contains(forbidden),
                "{name}: generated source contains {forbidden}"
            );
        }
        assert!(project.manifest.contains("ndarray"));
        assert!(project.manifest.contains("nalgebra"));
        // Helpers belong in the vendored crate, not in each generated file.
        assert!(
            project.source.len() < source.len() * 12 + 2048,
            "{name}: generated source is not compact"
        );
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (path, content) in project.files() {
            // Distinct package identities prevent Cargo from reusing another
            // fixture's binary when projects share a target cache and mtimes.
            let content = if path == "Cargo.toml" {
                assert!(content.contains(r#"name = "generated_matlab""#));
                content.replace(
                    r#"name = "generated_matlab""#,
                    &format!(r#"name = "generated_{}""#, name.replace('-', "_")),
                )
            } else {
                content
            };
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        if let Some(caller) = caller {
            std::fs::write(dir.join("src/main.rs"), caller).unwrap();
        }
        let output = Command::new("cargo")
            .args(["run", "--offline", "--quiet", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("RUSTFLAGS", "-Dwarnings")
            .env("CARGO_PROFILE_DEV_DEBUG", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success() == success,
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if name == "arguments" {
            let lint = Command::new("cargo")
                .args(["clippy", "--offline", "--quiet", "--manifest-path"])
                .arg(dir.join("Cargo.toml"))
                .args(["--all-targets", "--", "-Dwarnings"])
                .env("CARGO_TARGET_DIR", self.root.join("target"))
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_PROFILE_DEV_DEBUG", "0")
                .env("RUSTFLAGS", "-Dwarnings")
                .output()
                .unwrap();
            assert!(
                lint.status.success(),
                "generated scalar API clippy: {}",
                String::from_utf8_lossy(&lint.stderr)
            );
        }
        if success {
            String::from_utf8(output.stdout).unwrap()
        } else {
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                !error.contains("could not compile"),
                "{name}: generated source must compile: {error}"
            );
            error
        }
    }
}
impl Drop for Projects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn numbers(text: &str) -> Vec<f64> {
    text.split_whitespace()
        .map(|s| {
            s.parse()
                .unwrap_or_else(|_| panic!("non-numeric output: {text:?}"))
        })
        .collect()
}
fn assert_numbers(actual: &[f64], expected: &[f64], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}: output length");
    for (i, (&a, &b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a == b || a.is_nan() && b.is_nan() || (a - b).abs() <= 1e-10 * b.abs().max(1.0),
            "{context} element {i}: {a} != {b}"
        );
    }
}
fn value_numbers(value: &Value) -> Vec<f64> {
    let mut result = vec![value.rows as f64, value.cols as f64];
    result.extend_from_slice(&value.data);
    result
}
fn octave(source: &str) -> Option<Vec<f64>> {
    if Command::new("octave").arg("--version").output().is_err() {
        eprintln!("Octave unavailable; typed project reference comparison skipped");
        return None;
    }
    let result = Command::new("octave")
        .args(["--no-gui", "--quiet", "--eval", source])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Some(numbers(&String::from_utf8(result.stdout).unwrap()))
}
fn emit_result(name: &str) -> String {
    // Printing each column-major element avoids depending on pretty-printer layout.
    format!("disp(size({name}));\nfor print_i=1:numel({name})\ndisp({name}(print_i)+0);\nend\n")
}
fn octave_result(name: &str) -> String {
    format!(
        "fprintf('%.17g %.17g ',size({name},1),size({name},2));fprintf('%.17g ',double({name}(:)));fprintf('\\n');\n"
    )
}

#[test]
fn typed_projects_match_interpreter_and_octave() {
    let projects = Projects::new();
    typed_first_multioutput_assignments(&projects);
    typed_vector_indexed_assignments(&projects);
    typed_integer_format_validation(&projects);
    typed_library_exports_arrays_and_multiple_outputs_without_dynamic_environment(&projects);
    optional_typed_corpus(&projects);
    typed_codegen_edge_regressions(&projects);
    let bounds = projects.run_failure("bad-index", "A=[1 2];disp(A(0));");
    assert!(bounds.contains("indices"), "{bounds}");
    let unassigned = "if false\nx=1;\nend\ndisp(x);";
    if generate_project(unassigned, false).is_ok() {
        let error = projects.run_failure("unassigned", unassigned);
        assert!(!error.is_empty());
    }
    let commented = "y=f(3);disp(y);\nfunction y=f(x)\ny=x+1;\n%{\ny=999;\n%{\nsystem('must never execute');\n%}\ny=888;\n%}\ny=y*2;\nend\n";
    let result = numbers(&projects.run("comments", commented, false, None));
    assert_numbers(&result, &[8.], "nested comments");
    let comment_reference = format!(
        "{}\ny=f(3);disp(y);",
        commented.strip_prefix("y=f(3);disp(y);\n").unwrap()
    );
    if let Some(reference) = octave(&comment_reference) {
        assert_numbers(&result, &reference, "nested comments vs Octave");
    }
    let compact = generate_project("x=1;y=x+2;", false).unwrap();
    assert!(compact.source.lines().count() < 80);
    assert!(compact.source.contains("v_x: f64"));
    let logical = generate_project("mask=[1 2]>1;", false).unwrap();
    assert!(logical.source.contains("ArrayD<bool>"));
    let initial = "A=[1 2 3;4 5 6];v=[1 2 3];c=[4;5;6];s='abc';\n";
    let workspace = eval_script(initial, &BTreeMap::new()).unwrap();
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
        "2^3^2",
        "2^-2^2",
        "-2^-2",
        "1+2*3",
        "(1+2)*3",
        "mod(0.3,0.1)",
        "mod(-5,3)",
        "mod(5,-3)",
        "mod(2,0)",
        "mod(Inf,2)",
        "mod(2,Inf)",
        "sign(-0.0)",
        "round(-1.5)",
        "min(NaN,2)",
        "max(NaN,2)",
        "sqrt(4)",
        "sin(pi/2)",
        "exp(log(2))",
        "atan2(1,-1)",
        "1 < 2 < 3",
        "~2^0",
        "0 && NaN",
        "1 || NaN",
        "NaN ~= NaN",
        "A([true false true false true false])",
    ];
    let mut source = initial.to_owned();
    let mut reference = initial.to_owned();
    let mut expected = Vec::new();
    let mut octave_shape_difference = None;
    for (i, expression) in expressions.iter().enumerate() {
        let name = format!("result{i}");
        if *expression == "A([true false true false true false])" {
            octave_shape_difference = Some(expected.len());
        }
        source.push_str(&format!("{name}={expression};\n{}", emit_result(&name)));
        reference.push_str(&format!("{name}={expression};\n{}", octave_result(&name)));
        expected.extend(value_numbers(
            &eval_array_expr(expression, &workspace)
                .unwrap_or_else(|e| panic!("{expression}: {e}")),
        ));
    }
    let generated = numbers(&projects.run("expressions", &source, false, None));
    assert_numbers(&generated, &expected, "compiled expressions vs evaluator");
    if let Some(mut reference) = octave(&reference) {
        // MATLAB returns a column for matrix logical indexing. Octave preserves
        // a row mask's orientation here. Match MATLAB's documented shape while
        // still comparing all selected values to the independent Octave result.
        let offset = octave_shape_difference.unwrap();
        assert_eq!(&expected[offset..offset + 2], &[3., 1.]);
        assert_eq!(&reference[offset..offset + 2], &[1., 3.]);
        reference[offset..offset + 2].copy_from_slice(&expected[offset..offset + 2]);
        assert_numbers(&generated, &reference, "compiled expressions vs Octave");
        assert_numbers(&expected, &reference, "evaluator expressions vs Octave");
    }

    let script = "empty_i=7;\nfor empty_i=1:0\nend\nrow=[1 2];row(4)=7;\nM=[1 2;3 4];M(:,2)=[9;8];M(3,3)=5;\ntotal=0;\nfor j=1:5\ntotal=total+j;\nend\nk=0;\nwhile k<6\nk=k+1;\nif k==4\nbreak;\nend\nend\nif total==15\nanswer=M(:,end);\nelse\nanswer=[0;0;0];\nend\n";
    let workspace = eval_script(script, &BTreeMap::new()).unwrap();
    let mut source = script.to_owned();
    let mut reference = script.to_owned();
    let mut expected = Vec::new();
    for name in ["row", "M", "total", "k", "answer", "empty_i"] {
        source.push_str(&emit_result(name));
        reference.push_str(&octave_result(name));
        expected.extend(value_numbers(&workspace[name]));
    }
    let generated = numbers(&projects.run("control-flow", &source, false, None));
    assert_numbers(&generated, &expected, "compiled script vs interpreter");
    if let Some(reference) = octave(&reference) {
        assert_numbers(&generated, &reference, "compiled script vs Octave");
    }

    // The unchanged interpreter has no inv/det builtin; solve against eye is
    // an independent reference for inv, and this determinant is exactly five.
    let source = "A=[2 1;1 3];inverse=inv(A);determinant=det(A);\n";
    let mut generated_source = source.to_owned();
    let mut reference = source.to_owned();
    for name in ["inverse", "determinant"] {
        generated_source.push_str(&emit_result(name));
        reference.push_str(&octave_result(name));
    }
    let generated = numbers(&projects.run("linear-algebra", &generated_source, false, None));
    let mut expected =
        value_numbers(&eval_array_expr("[2 1;1 3]\\eye(2)", &BTreeMap::new()).unwrap());
    expected.extend([1., 1., 5.]);
    assert_numbers(&generated, &expected, "nalgebra inv/det");
    if let Some(reference) = octave(&reference) {
        assert_numbers(&generated, &reference, "nalgebra inv/det vs Octave");
    }
    typed_arguments_export_scalar_signature(&projects);
}

fn typed_library_exports_arrays_and_multiple_outputs_without_dynamic_environment(
    projects: &Projects,
) {
    let source = "function [last_column,first_row]=edges(x)\nlast_column=x(:,end);\nfirst_row=x(1,:);\nend\n";
    let project = generate_project(source, true).unwrap();
    assert!(project.library);
    assert!(project.source.contains("ArrayD<f64>"));
    assert!(!project.source.contains("fn main"));
    let caller = r#"
#[path="lib.rs"] mod generated;
use ndarray::{Array, ArrayD, IxDyn, ShapeBuilder};
fn print_array(a: &ArrayD<f64>) {
    assert_eq!(a.ndim(), 2);
    print!("{} {} ", a.shape()[0], a.shape()[1]);
    for c in 0..a.shape()[1] {
        for r in 0..a.shape()[0] {
            print!("{:.17} ", a[IxDyn(&[r,c])]);
        }
    }
    println!();
}
fn main() {
    let x = Array::from_shape_vec((2,3).f(), vec![1.,4.,2.,5.,3.,6.]).unwrap().into_dyn();
    let (a,b) = generated::f_edges(&x).unwrap();
    print_array(&a);
    print_array(&b);
    let scalar = Array::from_shape_vec((1,1).f(), vec![7.]).unwrap().into_dyn();
    let (a,b) = generated::f_edges(&scalar).unwrap();
    print_array(&a);
    print_array(&b);
}
"#;
    let actual = numbers(&projects.run("library", source, true, Some(caller)));
    let mut expected = Vec::new();
    for input in [
        Value::new(2, 3, vec![1., 4., 2., 5., 3., 6.]).unwrap(),
        Value::scalar(7.),
    ] {
        for value in unlinked_matlab::eval_function(source, vec![input]).unwrap() {
            expected.extend(value_numbers(&value));
        }
    }
    assert_numbers(&actual, &expected, "typed library vs evaluator");
    let reference = format!(
        "{source}\n[a,b]=edges([1 2 3;4 5 6]);\n{}{}[a,b]=edges(7);\n{}{}",
        octave_result("a"),
        octave_result("b"),
        octave_result("a"),
        octave_result("b")
    );
    if let Some(reference) = octave(&reference) {
        assert_numbers(&actual, &reference, "typed library vs Octave");
    }
}

fn optional_typed_corpus(projects: &Projects) {
    let Some(root) = std::env::var_os("UNLINKED_TEST_CASES") else {
        eprintln!("UNLINKED_TEST_CASES unset; typed corpus comparison skipped");
        return;
    };
    let root = PathBuf::from(root);
    let mut definitions = String::new();
    let mut pure_definitions = String::new();
    for file in [
        "uuv/functions/skew.m",
        "uuv/functions/rotation.m",
        "matlab/algorithms/Searching/binary_search.m",
        "matlab/algorithms/Searching/linear_search.m",
        "matlab/algorithms/Strings/isPalindrome.m",
        "matlab/algorithms/sorting/bubble_sort.m",
        "matlab/algorithms/maths/euclidean_distance.m",
        "matlab/algorithms/maths/find_factorial.m",
        "matlab/algorithms/maths/fibonacci_sequence.m",
    ] {
        let path = root.join("fixtures").join(file);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        definitions.push_str(&text);
        definitions.push('\n');
        // Only the pure interpreter oracle rejects disp. Compile and run the
        // ORIGINAL definitions; its test-only numeric oracle omits diagnostics.
        let has_diagnostics = text.lines().any(|line| line.trim().starts_with("disp("));
        if has_diagnostics {
            assert!(unlinked_matlab::FunctionProgram::parse(&text).is_err());
        }
        for line in text
            .lines()
            .filter(|line| !line.trim().starts_with("disp("))
        {
            pure_definitions.push_str(line);
            pure_definitions.push('\n');
        }
    }
    let mut source = String::new();
    let mut reference = format!("1;\n{definitions}");
    let mut expected = Vec::new();
    for (i, call) in [
        "skew([1;2;3])",
        "rotation([0.1;0.2;0.3],[1;2;3])",
        "binary_search([1 3 5 7 9],7)",
        "linear_search([1 3 5],3)",
        "linear_search([1 3 5],9)",
        "isPalindrome('racecar')",
        "isPalindrome('rust')",
        "bubble_sort([4 1 3 2 -1])",
        "euclidean_distance([1 2 3],[4 6 3])",
        "find_factorial(6)",
        "fibo(10)",
    ]
    .iter()
    .enumerate()
    {
        let name = format!("result{i}");
        source.push_str(&format!(
            "{name}={call};\ndisp('BEGIN_RESULT');\n{}disp('END_RESULT');\n",
            emit_result(&name)
        ));
        reference.push_str(&format!(
            "{name}={call};\ndisp('BEGIN_RESULT');\n{}disp('END_RESULT');\n",
            octave_result(&name)
        ));
        let wrapper = format!("function result=primary()\nresult={call};\nend\n{pure_definitions}");
        let values = unlinked_matlab::eval_function(&wrapper, vec![]).unwrap();
        expected.extend(value_numbers(&values[0]));
    }
    source.push_str(&definitions);
    let actual = marked_numbers(&projects.run("corpus", &source, false, None));
    assert_numbers(&actual, &expected, "typed corpus vs evaluator");
    if Command::new("octave").arg("--version").output().is_ok() {
        let script = projects.root.join("corpus.m");
        std::fs::write(&script, reference).unwrap();
        let output = Command::new("octave")
            .args(["--no-gui", "--quiet"])
            .arg(script)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reference = marked_numbers(&String::from_utf8(output.stdout).unwrap());
        assert_numbers(&actual, &reference, "typed corpus vs Octave");
    }
}
fn marked_numbers(text: &str) -> Vec<f64> {
    let mut result = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        match line.trim() {
            "BEGIN_RESULT" => {
                assert!(!inside);
                inside = true;
            }
            "END_RESULT" => {
                assert!(inside);
                inside = false;
            }
            _ if inside => result.extend(numbers(line)),
            _ => {}
        }
    }
    assert!(!inside);
    result
}

fn typed_codegen_edge_regressions(projects: &Projects) {
    for (name, expression, diagnostic) in [
        ("fractional-power", "(-1)^0.5", "complex powers"),
        ("nan-and", "NaN&1", "NaN cannot be converted"),
        ("nan-or", "NaN|0", "NaN cannot be converted"),
    ] {
        assert!(eval_array_expr(expression, &BTreeMap::new()).is_err());
        let error = projects.run_failure(name, &format!("y={expression};"));
        assert!(error.contains(diagnostic), "{name}: {error}");
    }
    let script = "[a]=zeros(1,2);[b]=sort([2 1]);[c]=two();\nlogical_result=reset(true);\niterations=0;empty_column=7;\nfor empty_column=zeros(0,3)\niterations=iterations+1;\nend\nA=[1 2];empty_selection=A(false);\n";
    let functions = "function [x,y]=two()\nx=1;y=2;\nend\nfunction y=reset(x)\nx=false;y=x;\nend\n";
    let mut source = script.to_owned();
    let mut reference = format!("{functions}\n{script}");
    let mut expected = Vec::new();
    let mut false_index_offset = 0;
    for name in [
        "a",
        "b",
        "c",
        "logical_result",
        "iterations",
        "empty_column",
        "empty_selection",
    ] {
        if name == "empty_selection" {
            false_index_offset = expected.len();
        }
        source.push_str(&emit_result(name));
        reference.push_str(&octave_result(name));
        let oracle = format!("function result=primary()\n{script}result={name};\nend\n{functions}");
        expected.extend(value_numbers(
            &unlinked_matlab::eval_function(&oracle, vec![]).unwrap()[0],
        ));
    }
    // The sandboxed evaluator intentionally requires variable names to begin
    // with a letter; standalone Octave-compatible generated code accepts `_`.
    let identifiers = "_=3;matlab_field__=4;disp(_);disp(matlab_field__);\n";
    assert!(eval_script("_=3;", &BTreeMap::new()).is_err());
    source.push_str(identifiers);
    reference.push_str(identifiers);
    expected.extend([3., 4.]);
    source.push_str(functions);
    let actual = numbers(&projects.run("codegen-edges", &source, false, None));
    assert_numbers(
        &actual,
        &expected,
        "typed codegen edge regressions vs evaluator",
    );
    if let Some(mut reference) = octave(&reference) {
        // The evaluator preserves the row orientation for A(false); Octave
        // produces 0x0 specifically for the scalar false index. Both are empty.
        assert_eq!(
            &expected[false_index_offset..false_index_offset + 2],
            &[1., 0.]
        );
        assert_eq!(
            &reference[false_index_offset..false_index_offset + 2],
            &[0., 0.]
        );
        reference[false_index_offset..false_index_offset + 2]
            .copy_from_slice(&expected[false_index_offset..false_index_offset + 2]);
        assert_numbers(
            &actual,
            &reference,
            "typed codegen edge regressions vs Octave",
        );
    }
}

fn typed_arguments_export_scalar_signature(projects: &Projects) {
    let source = "function y=polynomial(x)\narguments\nx (1,1) double\nend\ny=x^2+2*x+1;\nend\nfunction y=first_column(A)\narguments\nA (2,:) double\nend\ny=A(:,1);\nend\n";
    let caller = r#"
#[path="lib.rs"] mod generated;
fn main() {
    let polynomial: fn(f64) -> Result<f64, unlinked_matlab_rt::Error> = generated::f_polynomial;
    println!("{}", polynomial(3.0).unwrap());
    use ndarray::{Array, ShapeBuilder};
    let good = Array::from_shape_vec((2,2).f(),vec![1.,2.,3.,4.]).unwrap().into_dyn();
    let first = generated::f_first_column(&good).unwrap();
    assert_eq!(first.shape(), &[2,1]);
    assert_eq!(first.iter().copied().collect::<Vec<_>>(), vec![1.,2.]);
    let bad = Array::from_shape_vec((3,2).f(),vec![1.,2.,3.,4.,5.,6.]).unwrap().into_dyn();
    assert!(generated::f_first_column(&bad).is_err());
}
"#;
    let output = numbers(&projects.run("arguments", source, true, Some(caller)));
    assert_numbers(&output, &[16.], "arguments scalar signature");
    assert!(unlinked_matlab::eval_function(source, vec![Value::scalar(3.)]).is_err());
    for declaration in [
        "arguments\nx (1,1) double = 2\nend",
        "arguments\nx (1,1) double {mustBePositive}\nend",
        "arguments (Input)\nx (1,1) double\nend",
    ] {
        let invalid = format!("function y=f(x)\n{declaration}\ny=x;\nend\n");
        assert!(
            generate_project(&invalid, true).is_err(),
            "unsupported arguments syntax accepted: {declaration}"
        );
    }
    // The pure evaluator intentionally does not accept arguments declarations;
    // its unchanged numeric body remains the independent execution oracle.
    let oracle = "function y=polynomial(x)\ny=x^2+2*x+1;\nend\n";
    let expected = unlinked_matlab::eval_function(oracle, vec![Value::scalar(3.)]).unwrap();
    assert_numbers(
        &output,
        &expected[0].data,
        "arguments scalar result vs evaluator",
    );
    if let Some(reference) = octave(&format!("{oracle}\ndisp(polynomial(3));")) {
        assert_numbers(&output, &reference, "arguments scalar result vs Octave");
    }
}

fn typed_first_multioutput_assignments(projects: &Projects) {
    // Every target is first declared by a multi-output assignment and then read
    // outside the call's temporary-expression scope. Reassigning preexisting
    // variables would hide a generator that scoped `let` bindings too narrowly.
    let statements =
        "[r,c]=size(zeros(3,4));\n[a,b]=pair();\ncombined=combine();\n[d,e]=dimensions();\n";
    let functions = "function [x,y]=pair()\nx=2;y=7;\nend\nfunction result=combine()\n[u,v]=pair();\n[rows,cols]=size(ones(2,5));\nresult=u+v+rows+cols;\nend\nfunction [rows,cols]=dimensions()\n[rows,cols]=size(zeros(6,8));\nend\n";
    let output = "disp(r);disp(c);disp(a);disp(b);disp(combined);disp(d);disp(e);\n";
    let source = format!("{statements}{output}{functions}");
    let actual = numbers(&projects.run("first-multiple-outputs", &source, false, None));
    let oracle = format!(
        "function result=primary()\n{statements}result=[r c a b combined d e];\nend\n{functions}"
    );
    let expected = unlinked_matlab::eval_function(&oracle, vec![]).unwrap();
    assert_numbers(
        &actual,
        &[3., 4., 2., 7., 16., 6., 8.],
        "first multi-output assignments",
    );
    assert_numbers(
        &actual,
        &expected[0].data,
        "first multi-output assignments vs evaluator",
    );
    if let Some(reference) = octave(&format!("{functions}\n{statements}{output}")) {
        assert_numbers(
            &actual,
            &reference,
            "first multi-output assignments vs Octave",
        );
    }
}

// MathWorks: Detailed Rules for Indexed Assignment (nonsingleton dimensions
// must agree in order and length, with scalar expansion as a separate case).
fn typed_vector_indexed_assignments(projects: &Projects) {
    let cases = [
        "C=zeros(2,3);C(1,:)=[7;8;9];",
        "C=zeros(3,2);C(:,2)=[7 8 9];",
        "C=zeros(1,3);C(:,:)=[7;8;9];",
        "C=zeros(3,1);C(:,:)=[7 8 9];",
        "C=zeros(2,3);C(1,[true false true])=[7;9];",
        "C=zeros(2,3);C(1,[3 1 3])=[7;8;9];",
        "C=zeros(3,2);C([3 1 3],2)=[7 8 9];",
        "C=[1 2;3 4];C(3,[4 2])=[7;8];",
        "C=[1 2;3 4];C([4 2],3)=[7 8];",
        "C=zeros(2,3);C([2 1],[3 2 3])=[1 2 3;4 5 6];",
        "C=zeros(2,3);C([2 1],[3 1])=7;",
        "C=zeros(2,3);C([6 1 6])=[7;8;9];",
    ];
    let mut script = String::new();
    let mut reference_script = String::new();
    let mut expected = Vec::new();
    for source in cases {
        let workspace = eval_script(source, &BTreeMap::new()).unwrap();
        expected.extend(value_numbers(&workspace["C"]));
        script.push_str(source);
        script.push_str(&emit_result("C"));
        reference_script.push_str(source);
        reference_script.push_str(&octave_result("C"));
    }
    let actual = numbers(&projects.run("vector-assignment", &script, false, None));
    assert_numbers(&actual, &expected, "vector assignments vs evaluator");
    if let Some(reference) = octave(&reference_script) {
        assert_numbers(&actual, &reference, "vector assignments vs Octave");
    }
    // Equal element counts alone must not permit matrix/vector or transposed
    // matrix assignments. Wrong vector lengths must still fail as well.
    for (i, source) in [
        "C=zeros(2,3);C(:,:)=[1 2;3 4;5 6];",
        "C=zeros(2,3);C(:,:)=1:6;",
        "C=zeros(1,6);C(1,:)=reshape(1:6,2,3);",
        "C=zeros(2,3);C(1,:)=[7;8];",
    ]
    .into_iter()
    .enumerate()
    {
        let error = eval_script(source, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("shape mismatch"), "{error}");
        let error = projects.run_failure(&format!("vector-assignment-bad-{i}"), source);
        assert!(error.contains("shape mismatch"), "{error}");
        if let Some(result) = octave(&format!("try;{source}disp(0);catch;disp(1);end;")) {
            assert_eq!(result, vec![1.]);
        }
    }
}

fn typed_integer_format_validation(projects: &Projects) {
    let sprintf = |format: &str, values: Vec<Value>| {
        let mut args = vec![Value::string(format).unwrap()];
        args.extend(values);
        unlinked_matlab::array_runtime::builtin("sprintf", args, 1).map(|v| v[0].text().unwrap())
    };
    // The integer subset must neither truncate fractions nor saturate casts.
    // Modifiers on fractional integer conversions remain unsupported until
    // their MATLAB override behavior can be verified.
    for (i, (format, value, message)) in [
        ("%20d", "1.5", "noninteger %d/%i with width or precision"),
        ("%8.2i", "-1.5", "noninteger %d/%i"),
        ("%d", "NaN", "outside supported range"),
        ("%i", "Inf", "outside supported range"),
        ("%d", "-Inf", "outside supported range"),
        ("%d", "2^63", "outside supported range"),
        ("%d", "-2^63-2048", "outside supported range"),
        ("%d", "1e100", "outside supported range"),
    ]
    .into_iter()
    .enumerate()
    {
        let error = sprintf(
            format,
            vec![eval_array_expr(value, &BTreeMap::new()).unwrap()],
        )
        .unwrap_err();
        assert!(error.contains(message), "{error}");
        let source = format!("fprintf('{format}',{value});");
        let error = projects.run_failure(&format!("integer-format-bad-{i}"), &source);
        assert!(error.contains(message), "{error}");
    }
    // MATLAB's documented bare %e override differs from Octave's general
    // formatting: assert MATLAB text directly, and record Octave's divergence.
    for (i, (value, expected)) in [
        (1.5, "1.500000e+00"),
        (std::f64::consts::PI, "3.141593e+00"),
        (-1.5, "-1.500000e+00"),
        (0.015, "1.500000e-02"),
        (1.5e-100, "1.500000e-100"),
        (-1.5e-100, "-1.500000e-100"),
        (1e12 + 0.25, "1.000000e+12"),
        (-1e12 - 0.25, "-1.000000e+12"),
    ]
    .into_iter()
    .enumerate()
    {
        for spec in ["d", "i"] {
            let text = sprintf(&format!("%{spec}"), vec![Value::scalar(value)]).unwrap();
            assert_eq!(text, expected);
            let output = projects.run(
                &format!("fraction-integer-{i}-{spec}"),
                &format!("fprintf('%{spec}',{value:.17e});"),
                false,
                None,
            );
            assert_eq!(output, expected);
        }
    }
    if let Some(reference) = octave("s=sprintf('%d',1.5);fprintf('%d ',double(s));") {
        assert_eq!(reference, vec![49., 46., 53.]); // Octave gives '1.5'.
    }
    let source = "s=sprintf('<%d><%8i><%d><%d><%d><%d>',12,-12,0,-2^63,2^63-1024,-0);";
    let expected = "<12><     -12><0><-9223372036854775808><9223372036854774784><0>";
    let text = sprintf(
        "<%d><%8i><%d><%d><%d><%d>",
        [
            12.,
            -12.,
            0.,
            -9223372036854775808.,
            9223372036854774784.,
            -0.,
        ]
        .into_iter()
        .map(Value::scalar)
        .collect(),
    )
    .unwrap();
    assert_eq!(text, expected);
    let output = projects.run(
        "integer-format-valid",
        &format!("{source}fprintf('%s',s);"),
        false,
        None,
    );
    assert_eq!(output, expected);
    // Compare ASCII codes to retain padding and exact integral boundary digits.
    if let Some(reference) = octave(&format!("{source}fprintf('%d ',double(s));")) {
        assert_eq!(
            reference,
            expected.bytes().map(f64::from).collect::<Vec<_>>()
        );
    }
    // Explicit alternatives accept fractional values. %e currently uses Rust's
    // exponent spelling, so compare numeric values rather than claim text parity.
    for (i, (format, n)) in [("%g", 1.5), ("%.2e", -1.5)].into_iter().enumerate() {
        let expression = format!("sprintf('{format}',{n})");
        let text = sprintf(format, vec![Value::scalar(n)]).unwrap();
        let output = projects.run(
            &format!("fraction-format-{i}"),
            &format!("s={expression};fprintf('%s',s);"),
            false,
            None,
        );
        assert_eq!(output, text);
        assert_eq!(
            output.parse::<f64>().unwrap(),
            if i == 0 { 1.5 } else { -1.5 }
        );
        if let Some(reference) = octave(&format!("printf('%s',{expression});")) {
            assert_eq!(reference, vec![output.parse::<f64>().unwrap()]);
        }
    }
}
