use std::{collections::BTreeMap, process::Command};
use unlinked_matlab::{
    ArrayBudget,
    array_runtime::{Environment, Value},
    eval_script, eval_script_with_budget,
};

fn numeric(env: &Environment, name: &str, rows: usize, cols: usize, data: &[f64]) {
    let value = &env[name];
    assert_eq!((value.rows, value.cols), (rows, cols), "{name}");
    assert_eq!(value.data, data, "{name}");
}

#[test]
fn assignments_control_flow_and_indices_preserve_initial_workspace() {
    let initial = BTreeMap::from([("gain".into(), Value::scalar(3.))]);
    let source = "A=[1 2;3 4];A(:,end)=gain*[2;4];A(1,end+1)=9;\n\
        total=0;for column=A;total=total+sum(column);end\n\
        n=0;while n<10;n=n+1;if n==2;continue;elseif n==4;break;else;total=total+n;end;end\n\
        gain=gain+1;";
    let result = eval_script(source, &initial).unwrap();
    numeric(&result, "A", 2, 3, &[1., 3., 6., 12., 9., 0.]);
    numeric(&result, "total", 1, 1, &[35.]);
    numeric(&result, "n", 1, 1, &[4.]);
    numeric(&initial, "gain", 1, 1, &[3.]);
    numeric(&result, "gain", 1, 1, &[4.]);
    let grown = eval_script("x(3)=7;x(end+1)=x(3)+1;", &Environment::new()).unwrap();
    numeric(&grown, "x", 1, 4, &[0., 0., 7., 8.]);
}

#[test]
fn empty_ranges_nested_loops_and_short_circuit_conditions() {
    let result = eval_script(
        "i=9;for i=3:1;bad=undefined;end\n\
        result=0;for a=1:3;for b=1:4;if b==2;break;end;result=result+1;end;end\n\
        if false && missing;result=-1;elseif true || missing;result=result+10;end",
        &Environment::new(),
    )
    .unwrap();
    numeric(&result, "i", 1, 0, &[]);
    numeric(&result, "result", 1, 1, &[13.]);
}

#[test]
fn forbidden_capabilities_are_rejected_even_in_dead_branches() {
    for script in [
        "disp(1);",
        "fprintf('bad');",
        "x=sprintf('bad');",
        "system('bad');",
        "x=load('bad');",
        "if false; x=system('bad');end",
        "if false;disp(1);end",
        "function y=f(x);y=x;end",
        "a=1;function y=f(x);y=x;end",
        "[a,b]=size([1 2]);",
        "if false;[a,b]=size([1 2]);end",
        "break;",
        "continue;",
        "return;",
        "if false;return;end",
    ] {
        assert!(
            eval_script(script, &Environment::new()).is_err(),
            "{script}"
        );
    }
}

#[test]
fn script_limits_and_atomic_failures() {
    let initial = BTreeMap::from([("original".into(), Value::scalar(7.))]);
    assert!(eval_script("original=8;bad=zeros(1,1025);", &initial).is_err());
    numeric(&initial, "original", 1, 1, &[7.]);
    assert!(eval_script("original(1025)=1;", &initial).is_err());
    let names = (0..257).map(|i| format!("v{i}=1;")).collect::<String>();
    assert!(
        eval_script(&names, &Environment::new())
            .unwrap_err()
            .message
            .contains("256 variables")
    );
    let arrays = (0..98)
        .map(|i| format!("v{i}=ones(1,1024);"))
        .collect::<String>();
    assert!(
        eval_script(&arrays, &Environment::new())
            .unwrap_err()
            .message
            .contains("100000 elements")
    );
    assert!(
        eval_script("while true;end", &Environment::new())
            .unwrap_err()
            .message
            .contains("100000 steps")
    );
    assert!(
        eval_script("for i=1:100001;end", &Environment::new())
            .unwrap_err()
            .message
            .contains("100000 steps")
    );
    let large = BTreeMap::from([("x".into(), Value::new(1, 1025, vec![0.; 1025]).unwrap())]);
    assert!(eval_script("", &large).is_err());
    let malformed = BTreeMap::from([(
        "x".into(),
        Value {
            rows: 2,
            cols: 2,
            data: vec![],
            kind: unlinked_matlab::array_runtime::ValueKind::Numeric,
        },
    )]);
    assert!(eval_script("", &malformed).is_err());
    assert!(
        eval_script(
            "a=1;",
            &BTreeMap::from([("bad name".into(), Value::scalar(1.))])
        )
        .is_err()
    );
    let mut budget = ArrayBudget::with_limits(100, 100);
    assert!(eval_script_with_budget("while true;end", &Environment::new(), &mut budget).is_err());
    assert!(budget.remaining_operations() < 100);
    let mut budget = ArrayBudget::with_limits(8, 1000);
    let result = eval_script_with_budget("a=ones(2);", &Environment::new(), &mut budget).unwrap();
    assert!(unlinked_matlab::eval_array_expr_with_budget("a+a", &result, &mut budget).is_err());
}

#[test]
fn pure_initialization_scripts_match_octave() {
    if Command::new("octave").arg("--version").output().is_err() {
        eprintln!("Octave unavailable; initialization differential skipped");
        return;
    }
    let cases = [
        "result=[1 2;3 4];result(:,end)=gain*[2;4];result(1,end+1)=9;",
        "result=zeros(2,2);for col=[1 2;3 4];result=result+col;end",
        "result=0;i=0;while i<6;i=i+1;if i==2;continue;end;if i==5;break;end;result=result+i;end",
        "result=8;for result=3:1;end",
        "result(4)=7;result(end+1)=9;",
        "A=[1 2;3 4];result=A(A>2);",
        "if false;result=0;elseif gain>2;result=eye(3);else;result=ones(2);end",
        "result=0;for i=1:4;for j=1:3;if j==2;break;end;result=result+i;end;end",
    ];
    let initial = BTreeMap::from([("gain".into(), Value::scalar(3.))]);
    for script in cases {
        let ours = eval_script(script, &initial).unwrap();
        let actual = &ours["result"];
        let source = format!(
            "gain=3;{script}\n fprintf('%d %d ',rows(result),columns(result));fprintf('%.17g ',result(:));"
        );
        let run = Command::new("octave")
            .args(["--quiet", "--no-gui", "--eval", &source])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let data = String::from_utf8(run.stdout)
            .unwrap()
            .split_whitespace()
            .map(|x| x.parse::<f64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            (actual.rows, actual.cols),
            (data[0] as usize, data[1] as usize),
            "{script}"
        );
        assert_eq!(actual.data, &data[2..], "{script}");
    }
}

#[test]
fn empty_matrix_linear_growth_matches_octave_errors() {
    let cases = [
        (0, 0, true),
        (0, 1, true),
        (1, 0, true),
        (2, 1, true),
        (1, 2, true),
        (2, 0, false),
        (3, 0, false),
    ];
    let octave_available = Command::new("octave").arg("--version").output().is_ok();
    for (rows, cols, allowed) in cases {
        let source = format!("x=zeros({rows},{cols});x(3)=1;");
        let ours = eval_script(&source, &BTreeMap::new());
        assert_eq!(ours.is_ok(), allowed, "{source}");
        if octave_available {
            let probe = format!(
                "try;{source}fprintf('OK %d %d ',rows(x),columns(x));fprintf('%.17g ',x(:));catch;fprintf('ERROR');end;"
            );
            let result = Command::new("octave")
                .args(["--quiet", "--no-gui", "--eval", &probe])
                .output()
                .unwrap();
            assert!(result.status.success());
            let text = String::from_utf8(result.stdout).unwrap();
            if let Ok(workspace) = ours {
                let array = &workspace["x"];
                let expected = text
                    .strip_prefix("OK ")
                    .expect("Octave should accept shared subset")
                    .split_whitespace()
                    .map(|n| n.parse::<f64>().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(
                    (array.rows, array.cols),
                    (expected[0] as usize, expected[1] as usize),
                    "{source}"
                );
                assert_eq!(array.data, expected[2..], "{source}");
            } else {
                assert_eq!(text, "ERROR", "{source}");
            }
        }
    }
    if !octave_available {
        eprintln!("Octave unavailable; growth error differential skipped");
    }
    // General non-vector linear growth remains outside the supported subset,
    // including zero-row shapes accepted by some Octave versions.
    assert!(eval_script("x=zeros(0,2);x(3)=1;", &BTreeMap::new()).is_err());
}
