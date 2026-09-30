use std::{collections::BTreeMap, process::Command};
use unlinked_matlab::{
    array_runtime::Value, eval_function, eval_script, transpile, transpile_arrays,
};

const FUNCTION: &str = "function y=f(x)\ny=x+1;\n  %{ \n y=999;\n %{\n system('must never execute');\n %}\n y=888;\n  %} \ny=y*2;\nend\n";

#[test]
fn nested_comments_do_not_execute_or_require_capabilities() {
    assert_eq!(
        eval_function(FUNCTION, vec![Value::scalar(3.)]).unwrap()[0].data,
        [8.]
    );
    let script = "x=2;\n%{\nx=999;\n%{\nbad invalid source ' {\n%}\n%}\nx=x+1;";
    assert_eq!(
        eval_script(script, &BTreeMap::new()).unwrap()["x"].data,
        [3.]
    );
    let strings = eval_script("x='%{'; y='%}';", &BTreeMap::new()).unwrap();
    assert_eq!(strings["x"].data, [37., 123.]);
    assert_eq!(strings["y"].data, [37., 125.]);
    // Closing delimiters inside ordinary line comments are not block markers.
    assert_eq!(
        eval_script("x=1; % ordinary %{ and %}\nx=x+1;", &BTreeMap::new()).unwrap()["x"].data,
        [2.]
    );
}

#[test]
fn malformed_unclosed_and_overdeep_block_comments_are_diagnostics() {
    for source in [
        "x=1; %{\nx=2;\n%}\n",
        "%{ trailing text\nx=1;\n%}\n",
        "%}\nx=1;",
        "%{\nx=1;",
        "%{\nx=1;\n%} trailing text\n",
    ] {
        assert!(eval_script(source, &BTreeMap::new()).is_err(), "{source}");
        assert!(transpile(source).is_err(), "{source}");
        assert!(transpile_arrays(source, false).is_err(), "{source}");
        let function = format!("function y=f()\ny=1;\n{source}\nend");
        assert!(eval_function(&function, vec![]).is_err(), "{source}");
    }
    let source = format!("{}{}x=1;", "%{\n".repeat(65), "%}\n".repeat(65));
    let error = eval_script(&source, &BTreeMap::new()).unwrap_err();
    assert!(error.to_string().contains("64 levels"));
    let source = format!("{}{}x=1;", "%{\n".repeat(64), "%}\n".repeat(64));
    assert_eq!(
        eval_script(&source, &BTreeMap::new()).unwrap()["x"].data,
        [1.]
    );
    let error = eval_script("%{\nignored\n%}\nx=@bad;", &BTreeMap::new()).unwrap_err();
    assert_eq!(error.line, 4);
}

#[test]
fn generated_rust_and_octave_skip_the_same_nested_function_comments() {
    let directory = std::env::temp_dir().join(format!("unlinked-comments-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = format!("y=f(3);disp(y);\n{FUNCTION}");
    // Both native scalar ABI and array ABI use the same lexical comment rules.
    for (index, rust) in [
        transpile(&source).unwrap(),
        transpile_arrays(&source, false).unwrap(),
    ]
    .iter()
    .enumerate()
    {
        let file = directory.join(format!("comments{index}.rs"));
        let executable = directory.join(format!("comments{index}"));
        std::fs::write(&file, rust).unwrap();
        let compiled = Command::new("rustc")
            .args(["--edition=2024"])
            .arg(&file)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let result = Command::new(&executable).output().unwrap();
        assert!(result.status.success());
        assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "8");
    }
    if Command::new("octave").arg("--version").output().is_ok() {
        std::fs::write(directory.join("f.m"), FUNCTION).unwrap();
        let result = Command::new("octave")
            .current_dir(&directory)
            .args(["--quiet", "--no-gui", "--eval", "fprintf('%.17g',f(3));"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8(result.stdout).unwrap(), "8");
    } else {
        eprintln!("Octave unavailable; nested comment differential skipped");
    }
    std::fs::remove_dir_all(directory).unwrap();
}
