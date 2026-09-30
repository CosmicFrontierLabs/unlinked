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
