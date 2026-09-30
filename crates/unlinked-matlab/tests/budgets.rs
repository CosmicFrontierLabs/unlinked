use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use unlinked_matlab::array_runtime::Value;
use unlinked_matlab::{
    ArrayBudget, eval_array_expr_with_budget, eval_script, eval_script_with_budget,
};

#[test]
fn empty_dimensions_count_toward_parameter_work() {
    let env = BTreeMap::from([("a".into(), Value::new(0, 1_000_000, vec![]).unwrap())]);
    let source = format!("[{}]", vec!["a"; 8000].join(";"));
    let mut budget = ArrayBudget::default();
    let error = eval_array_expr_with_budget(&source, &env, &mut budget).unwrap_err();
    assert!(error.to_string().contains("operation budget"), "{error}");
    let mut budget = ArrayBudget::with_limits(100, 100);
    assert!(eval_array_expr_with_budget("zeros(0,1000)", &BTreeMap::new(), &mut budget).is_err());
}
#[test]
fn stored_empty_shapes_have_dimension_limits() {
    for source in ["a=zeros(0,1000000);", "a=zeros(1000000,0);"] {
        let error = eval_script(source, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("dimensions exceed"), "{error}");
    }
    let env = BTreeMap::from([("a".into(), Value::new(0, 1000000, vec![]).unwrap())]);
    assert!(eval_script("", &env).is_err());
}
#[test]
fn interruption_checks_entry_expressions_and_statement_loops() {
    for source in ["", "a=1;"] {
        let mut budget = ArrayBudget::default().with_cancellation(|| true);
        assert!(
            eval_script_with_budget(source, &BTreeMap::new(), &mut budget)
                .unwrap_err()
                .to_string()
                .contains("execution interrupted")
        );
    }
    let mut budget = ArrayBudget::default().with_cancellation(|| true);
    assert!(
        eval_array_expr_with_budget("1", &BTreeMap::new(), &mut budget)
            .unwrap_err()
            .to_string()
            .contains("execution interrupted")
    );
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = checks.clone();
    let mut budget = ArrayBudget::default()
        .with_cancellation(move || observed.fetch_add(1, Ordering::Relaxed) > 100);
    assert!(
        eval_script_with_budget("while true\nend", &BTreeMap::new(), &mut budget)
            .unwrap_err()
            .to_string()
            .contains("execution interrupted")
    );
    assert!(checks.load(Ordering::Relaxed) > 100);
}
