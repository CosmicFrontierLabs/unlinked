use std::{collections::BTreeMap, process::Command};
use unlinked_matlab::{eval_expr, transpile};

#[test]
fn expression_semantics() {
    let vars = BTreeMap::from([("gain".into(), 3.0)]);
    for (source, expected) in [
        ("gain * 2 + 1", 7.0),
        ("-2^2", -4.0),
        ("2^3^2", 64.0),
        ("2^-2", 0.25),
        ("mod(-5,3)", 1.0),
        ("sign(0)", 0.0),
        ("sin(pi/2)", 1.0),
        ("false && missing", 0.0),
        ("true || missing", 1.0),
    ] {
        assert_eq!(eval_expr(source, &vars).unwrap(), expected, "{source}");
    }
    assert!(eval_expr("[1,2]", &vars).is_err());
    assert!(eval_expr("gain = 2", &vars).is_err());
    assert!(eval_expr("system(1)", &vars).is_err());
}

#[test]
fn diagnostics_reject_unsupported_and_unbound() {
    for source in [
        "a = [1 2]",
        "disp(missing)",
        "system('echo hi')",
        "while 1\nend",
        "a = unknown(2)",
        "x = sin(1,2)",
        "if 1\na=2\nend\ndisp(a)",
        "for i=1:0\nx=1\nend\ndisp(x)",
        "function y=f(x)\nend",
        "function y=f(x,x)\ny=x\nend",
    ] {
        assert!(transpile(source).is_err(), "accepted {source}");
    }
    let error = transpile("a = 1;\nb = [2];").unwrap_err();
    assert_eq!(error.line, 2);
}

#[test]
fn generated_rust_compiles_runs_and_emits_llvm() {
    // Only a fixed, repository-owned fixture is compiled/executed by this test.
    let source = r#"
% fixed scalar control-flow and local-function regression fixture
sum = 0;
for i = 1:5
  sum = sum + square(i);
end
if sum < 0
  answer = 1;
elseif sum == 55
  answer = sum + mod(-5, 3);
else
  answer = 2;
end
disp(answer);
disp(-2^2);
disp(2^3^2);
for j = 3:-1:1
  disp(j);
end
function y = square(x)
  y = x^2;
end
"#;
    let dir = std::env::temp_dir().join(format!("unlinked-matlab-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rust_path = dir.join("fixture.rs");
    std::fs::write(&rust_path, transpile(source).unwrap()).unwrap();
    let executable = dir.join("fixture");
    let compile = Command::new("rustc")
        .args(["--edition=2024", "--emit=link,llvm-ir"])
        .arg(&rust_path)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(dir.join("fixture.ll").exists());
    let run = Command::new(&executable).output().unwrap();
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        "56\n-4\n64\n3\n2\n1\n"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bounded_parser_rejects_deep_or_large_input() {
    let vars = BTreeMap::new();
    assert!(eval_expr(&format!("{}1{}", "(".repeat(70), ")".repeat(70)), &vars).is_err());
    assert!(eval_expr(&"1+".repeat(1000), &vars).is_err());
    assert!(transpile(&"\n".repeat(2000)).is_err());
    assert!(transpile(&" ".repeat(70_000)).is_err());
}

#[test]
fn function_file_exports_callable_library() {
    let source = "function y = polynomial(x)\ny = x^2 + 2*x + 1;\nend";
    let generated = unlinked_matlab::transpile_library(source).unwrap();
    assert!(!generated.contains("fn main"));
    assert!(unlinked_matlab::transpile_library("x = 2;").is_err());
    let dir = std::env::temp_dir().join(format!("unlinked-matlab-library-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("generated.rs"), generated).unwrap();
    std::fs::write(
        dir.join("caller.rs"),
        "mod generated; fn main() { assert_eq!(generated::f_polynomial(3.0), 16.0); }",
    )
    .unwrap();
    let executable = dir.join("caller");
    let result = Command::new("rustc")
        .arg(dir.join("caller.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
    let result = Command::new("rustc")
        .args(["--crate-type=lib", "--emit=llvm-ir,link"])
        .arg(dir.join("generated.rs"))
        .arg("--out-dir")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(dir.join("generated.ll").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nan_logical_conversion_rejected_but_comparisons_allowed() {
    let vars = BTreeMap::new();
    for source in ["~NaN", "NaN && 1", "NaN || 0", "1 && NaN", "0 || NaN"] {
        assert!(eval_expr(source, &vars).is_err(), "{source}");
    }
    assert_eq!(eval_expr("NaN == NaN", &vars).unwrap(), 0.0);
    assert_eq!(eval_expr("NaN ~= NaN", &vars).unwrap(), 1.0);
    assert_eq!(eval_expr("1 || NaN", &vars).unwrap(), 1.0);
    assert_eq!(eval_expr("0 && NaN", &vars).unwrap(), 0.0);
}

#[test]
fn octave_differential_scalar_and_generated_rust() {
    if Command::new("octave").arg("--version").output().is_err() {
        eprintln!("Octave unavailable; differential conformance test skipped");
        return;
    }
    // Every expression is a fixed repository fixture, never uploaded input.
    let expressions = [
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
    ];
    let octave_script: String = expressions
        .iter()
        .map(|e| format!("fprintf('%.17g\\n', {e});\n"))
        .collect();
    let reference = Command::new("octave")
        .args(["--no-gui", "--quiet", "--eval", &octave_script])
        .output()
        .unwrap();
    assert!(
        reference.status.success(),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );
    let expected: Vec<f64> = String::from_utf8(reference.stdout)
        .unwrap()
        .lines()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(expected.len(), expressions.len());
    let source: String = expressions
        .iter()
        .map(|e| format!("disp({e});\n"))
        .collect();
    let dir = std::env::temp_dir().join(format!("unlinked-matlab-octave-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fixture.rs"), transpile(&source).unwrap()).unwrap();
    let executable = dir.join("fixture");
    let result = Command::new("rustc")
        .arg(dir.join("fixture.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let run = Command::new(&executable).output().unwrap();
    assert!(run.status.success());
    let actual: Vec<f64> = String::from_utf8(run.stdout)
        .unwrap()
        .lines()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(actual.len(), expressions.len());
    for ((expression, expected), compiled) in expressions.iter().zip(expected).zip(actual) {
        let evaluated = eval_expr(expression, &BTreeMap::new()).unwrap();
        for value in [evaluated, compiled] {
            assert!(
                (value.is_nan() && expected.is_nan())
                    || value == expected
                    || (value - expected).abs() <= 1e-13 * expected.abs().max(1.0),
                "{expression}: {value} != {expected}"
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn arbitrary_malformed_input_does_not_panic() {
    let alphabet = b"xyz0123+-*/^()=:;,\n%[]'~&|\\";
    let mut seed = 0x12345678_u64;
    for length in 0..128 {
        for _ in 0..20 {
            let source: String = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    alphabet[(seed >> 32) as usize % alphabet.len()] as char
                })
                .collect();
            let result = std::panic::catch_unwind(|| {
                let _ = transpile(&source);
                let _ = eval_expr(&source, &BTreeMap::new());
            });
            assert!(result.is_ok(), "parser panic for {source:?}");
        }
    }
    // A flat chain constructs a deep left-associated AST without nested parentheses.
    let long_chain = vec!["1"; 500].join("+");
    assert_eq!(eval_expr(&long_chain, &BTreeMap::new()).unwrap(), 500.0);
    assert!(transpile(&format!("x={long_chain};")).is_ok());
}

#[test]
fn empty_for_range_rejects_array_semantics_instead_of_preserving_scalar() {
    // Octave assigns [] to i here. The scalar subset cannot represent that value.
    let source = "i=7;\nfor i=1:0\nend\ndisp(i);\n";
    if Command::new("octave").arg("--version").output().is_ok() {
        let reference = Command::new("octave")
            .args([
                "--no-gui",
                "--quiet",
                "--eval",
                "i=7; for i=1:0; end; assert(isempty(i));",
            ])
            .output()
            .unwrap();
        assert!(
            reference.status.success(),
            "{}",
            String::from_utf8_lossy(&reference.stderr)
        );
    } else {
        eprintln!("Octave unavailable; empty-loop reference check skipped");
    }
    let dir = std::env::temp_dir().join(format!(
        "unlinked-matlab-empty-range-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fixture.rs"), transpile(source).unwrap()).unwrap();
    let executable = dir.join("fixture");
    let result = Command::new("rustc")
        .arg(dir.join("fixture.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = Command::new(&executable).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("empty for range requires an array-valued loop variable")
    );
    std::fs::remove_dir_all(dir).unwrap();
}
