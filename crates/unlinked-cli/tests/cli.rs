use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_unlinked"))
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scalar.mdl")
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn stdin(args: &[&str], source: &[u8]) -> Output {
    let mut child = binary()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn import_metadata_and_simulation_outputs() {
    let info = binary().arg("info").arg(fixture()).output().unwrap();
    success(&info);
    let json: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(json["name"], "scalar");
    assert_eq!(json["blocks"], 3);
    assert_eq!(json["block_types"]["Gain"], 1);
    assert_eq!(json["systems"][0]["connections"], 2);
    let trace = binary()
        .arg("sim")
        .arg(fixture())
        .args(["--stop", "0.2", "--step", "0.1", "--solver", "rk4"])
        .output()
        .unwrap();
    success(&trace);
    let json: serde_json::Value = serde_json::from_slice(&trace.stdout).unwrap();
    assert_eq!(json["trace"]["time"], serde_json::json!([0.0, 0.1, 0.2]));
    assert_eq!(json["options"]["solver"], "rk4");
    assert!(
        json["trace"]["signals"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| *v == serde_json::json!([6.0, 6.0, 6.0]))
    );
    let csv = binary()
        .arg("sim")
        .arg(fixture())
        .args([
            "--stop", "0.2", "--step", "0.1", "--solver", "euler", "--format", "csv",
        ])
        .output()
        .unwrap();
    success(&csv);
    let mut reader = csv::Reader::from_reader(csv.stdout.as_slice());
    assert_eq!(reader.headers().unwrap().get(0), Some("time"));
    let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row.iter().filter(|v| *v == "6").count() == 2)
    );
}
#[test]
fn malformed_model_and_missing_solver_are_errors() {
    let invalid = stdin(&["info", "-"], b"not a model");
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    let missing = binary()
        .arg("sim")
        .arg(fixture())
        .args(["--stop", "1", "--step", "0.1"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--solver"));
    let invalid = binary()
        .arg("sim")
        .arg(fixture())
        .args(["--stop", "1", "--step", "0", "--solver", "rk4"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
}
#[test]
fn transpile_script_library_and_llvm_without_execution() {
    let script = stdin(&["transpile", "-"], b"disp(1+2);\n");
    success(&script);
    assert!(String::from_utf8_lossy(&script.stdout).contains("fn main()"));
    let library = stdin(
        &["transpile", "-", "--library"],
        b"function y=f(x)\ny=x^2;\nend\n",
    );
    success(&library);
    let generated = String::from_utf8(library.stdout).unwrap();
    assert!(generated.contains("pub fn f_f"));
    assert!(!generated.contains("fn main"));
    let path = std::env::temp_dir().join(format!("unlinked-cli-{}.ll", std::process::id()));
    let output = path.to_str().unwrap();
    let llvm = stdin(
        &["transpile", "-", "--emit", "llvm-ir", "-o", output],
        b"disp(1+2);\n",
    );
    success(&llvm);
    assert!(llvm.stdout.is_empty());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("target triple")
    );
    std::fs::remove_file(path).unwrap();
    let invalid = stdin(&["transpile", "-"], b"system('echo unsafe')");
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
}
#[test]
fn input_size_limit_is_enforced_before_import() {
    let path =
        std::env::temp_dir().join(format!("unlinked-cli-oversized-{}.mdl", std::process::id()));
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let result = binary().arg("info").arg(&path).output().unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("16 MiB"));
}

#[test]
fn workspace_injection_resolves_dependencies_and_rejects_ambiguity() {
    let source = std::fs::read_to_string(fixture())
        .unwrap()
        .replace("Gain \"3\"", "Gain \"gain_value\"");
    let result = stdin(
        &[
            "sim",
            "-",
            "--stop",
            "0.1",
            "--step",
            "0.1",
            "--solver",
            "rk4",
            "--var",
            "gain_value=base*2",
            "--var",
            "base=3",
        ],
        source.as_bytes(),
    );
    success(&result);
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["workspace"]["gain_value"], "base*2");
    assert!(
        json["trace"]["signals"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| *v == serde_json::json!([12.0, 12.0]))
    );
    for definitions in [
        vec!["K=1", "K=2"],
        vec!["_invalid=2"],
        vec!["for=2"],
        vec!["K="],
        vec!["K=missing"],
        vec!["K"],
    ] {
        let mut args = vec![
            "sim", "-", "--stop", "0.1", "--step", "0.1", "--solver", "rk4",
        ];
        for definition in definitions {
            args.extend(["--var", definition]);
        }
        let result = stdin(&args, include_bytes!("fixtures/scalar.mdl"));
        assert!(!result.status.success(), "accepted {args:?}");
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn optional_corpus_analytic_fixture() {
    let Some(root) = std::env::var_os("UNLINKED_TEST_CASES") else {
        eprintln!("UNLINKED_TEST_CASES unset; external corpus analytic CLI test skipped");
        return;
    };
    let root = PathBuf::from(root);
    let model_path = root.join("fixtures/synthetic/constant_gain_sum_integrator.mdl");
    let expected_path = root.join("fixtures/synthetic/constant_gain_sum_integrator.expected.json");
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&expected_path).unwrap_or_else(|e| {
            panic!(
                "UNLINKED_TEST_CASES is set but {} cannot be read: {e}",
                expected_path.display()
            )
        }))
        .unwrap();
    let bytes = std::fs::read(&model_path).unwrap_or_else(|e| {
        panic!(
            "UNLINKED_TEST_CASES is set but {} cannot be read: {e}",
            model_path.display()
        )
    });
    let model = unlinked_import::import("constant_gain_sum_integrator.mdl", &bytes).unwrap();
    let output = binary()
        .arg("sim")
        .arg(&model_path)
        .args([
            "--start", "0", "--stop", "1", "--step", "0.1", "--solver", "rk4",
        ])
        .output()
        .unwrap();
    success(&output);
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tolerance = expected["absolute_tolerance"].as_f64().unwrap();
    let compare = |label: &str, expected: &serde_json::Value, actual: &serde_json::Value| {
        let expected = expected.as_array().unwrap();
        let actual = actual
            .as_array()
            .unwrap_or_else(|| panic!("missing trace {label}"));
        assert_eq!(actual.len(), expected.len(), "{label} sample count");
        for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
            let a = a.as_f64().unwrap();
            let e = e.as_f64().unwrap();
            assert!(
                (a - e).abs() <= tolerance,
                "{label} sample {index}: {a} != {e}"
            );
        }
    };
    compare("time", &expected["time"], &actual["trace"]["time"]);
    for (name, expected) in expected["signals"].as_object().unwrap() {
        let block = model.root.block_by_name(name).unwrap();
        compare(name, expected, &actual["trace"]["signals"][&block.id.0]);
    }
}
