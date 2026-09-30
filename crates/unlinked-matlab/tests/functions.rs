use std::collections::BTreeMap;
use unlinked_matlab::array_runtime::{Value, ValueKind};
use unlinked_matlab::{ArrayBudget, FunctionProgram, eval_function, eval_script};

#[test]
fn primary_signature_multiple_outputs_local_calls_and_indexing() {
    let source = "function [y,z]=primary(x,n)\ny=helper(x(:,end));\n[r,c]=size(x);\nz=r+c+n;\nend\nfunction y=helper(x)\ny=x.^2;\nend";
    let program = FunctionProgram::parse(source).unwrap();
    assert_eq!(program.signature().name, "primary");
    assert_eq!(program.signature().inputs, ["x", "n"]);
    assert_eq!(program.signature().outputs, ["y", "z"]);
    let result = program
        .evaluate(vec![
            Value::new(2, 2, vec![1., 2., 3., 4.]).unwrap(),
            Value::scalar(7.),
        ])
        .unwrap();
    assert_eq!(result[0].data, [9., 16.]);
    assert_eq!((result[0].rows, result[0].cols), (2, 1));
    assert_eq!(result[1].data, [11.]);
    assert!(program.evaluate(vec![]).is_err());
}
#[test]
fn loop_control_return_and_recursive_functions() {
    let source = "function y=f(n)\ny=0;\nfor k=1:n\nif k==2\ncontinue;\nend\nif k==4\nreturn;\nend\ny=y+k;\nend\nend";
    assert_eq!(
        eval_function(source, vec![Value::scalar(8.)]).unwrap()[0].data,
        [4.]
    );
    let source = "function y=f(n)\nif n<=1\ny=1;\nelse\ny=n*f(n-1);\nend\nend";
    assert_eq!(
        eval_function(source, vec![Value::scalar(6.)]).unwrap()[0].data,
        [720.]
    );
    assert!(
        eval_function(source, vec![Value::scalar(100.)])
            .unwrap_err()
            .to_string()
            .contains("recursion")
    );
}
#[test]
fn untrusted_capabilities_and_dead_branches_reject() {
    for expression in [
        "system('bad')",
        "fprintf('bad')",
        "disp(1)",
        "load('bad')",
        "eval('1')",
    ] {
        let source = format!("function y=f()\ny=1;\nif false\nz={expression};\nend\nend");
        assert!(FunctionProgram::parse(&source).is_err(), "{expression}");
    }
    for source in [
        "x=1;\nfunction y=f()\ny=1;\nend",
        "function y=f()\nbreak;\ny=1;\nend",
        "function y=f()\ny=1;\nend\nfunction z=g()\nz=system('bad');\nend",
    ] {
        assert!(FunctionProgram::parse(source).is_err(), "{source}");
    }
    assert!(eval_script("function y=f()\ny=1;\nend", &BTreeMap::new()).is_err());
}
#[test]
fn malformed_values_undefined_outputs_and_work_are_bounded() {
    let identity = FunctionProgram::parse("function y=f(x)\ny=x;\nend").unwrap();
    assert!(
        identity
            .evaluate(vec![Value {
                rows: 2,
                cols: 2,
                data: vec![],
                kind: ValueKind::Numeric
            }])
            .is_err()
    );
    assert!(
        identity
            .evaluate(vec![Value::row(&vec![0.; 1025]).unwrap()])
            .is_err()
    );
    assert!(eval_function("function y=f()\nif false\ny=1;\nend\nend", vec![]).is_err());
    assert!(
        eval_function("function y=f()\ny=0;\nwhile true\ny=y+1;\nend\nend", vec![])
            .unwrap_err()
            .to_string()
            .contains("steps")
    );
    let mut budget = ArrayBudget::with_limits(100, 3);
    assert!(
        identity
            .evaluate_with_budget(vec![Value::scalar(2.)], &mut budget)
            .is_err()
    );
    for source in [
        "function",
        "function [",
        "function y=f(",
        "function y=f()\ny=(;\nend",
    ] {
        assert!(FunctionProgram::parse(source).is_err());
    }
}

#[test]
fn pure_diagnostics_do_not_perform_io() {
    let source = "function y=f(x)\nassert(x>0,'positive required');\nif x>10\nerror('too large: %g',x);\nend\ny=sprintf('value=%g',x);\nend";
    assert_eq!(
        eval_function(source, vec![Value::scalar(2.)]).unwrap()[0]
            .text()
            .unwrap(),
        "value=2"
    );
    assert!(
        eval_function(source, vec![Value::scalar(-1.)])
            .unwrap_err()
            .to_string()
            .contains("positive required")
    );
    assert!(
        eval_function(source, vec![Value::scalar(20.)])
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
}

#[test]
fn combined_call_and_expression_depth_and_interruptions_are_bounded() {
    let expression = format!("f(n-1){}", "+1".repeat(180));
    let source = format!("function y=f(n)\nif n<=0\ny=0;\nelse\ny={expression};\nend\nend");
    let error = eval_function(&source, vec![Value::scalar(100.)]).unwrap_err();
    assert!(error.to_string().contains("stack budget"), "{error}");
    let program = FunctionProgram::parse("function f()\nend").unwrap();
    let mut budget = ArrayBudget::default().with_cancellation(|| true);
    assert!(
        program
            .evaluate_with_budget(vec![], &mut budget)
            .unwrap_err()
            .to_string()
            .contains("execution interrupted")
    );
    assert!(eval_function("function y=f()\ny=zeros(0,1000000);\nend", vec![]).is_err());
}
