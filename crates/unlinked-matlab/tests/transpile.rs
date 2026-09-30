use std::collections::BTreeMap;
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
        "a = {1 2}",
        "disp(missing)",
        "system('echo hi')",
        "switch 1\nend",
        "a = unknown(2)",
        "function y=f(x)\nend",
        "function y=f(x,x)\ny=x\nend",
    ] {
        assert!(transpile(source).is_err(), "accepted {source}");
    }
    let error = transpile("a = 1;\nb = [2,,3];").unwrap_err();
    assert_eq!(error.line, 2);
}

#[test]
fn bounded_parser_rejects_deep_or_large_input() {
    let vars = BTreeMap::new();
    assert!(eval_expr(&format!("{}1{}", "(".repeat(70), ")".repeat(70)), &vars).is_err());
    assert!(eval_expr(&"1+".repeat(1000), &vars).is_err());
    assert!(transpile(&"\n".repeat(20_000)).is_err());
    assert!(transpile(&" ".repeat(270_000)).is_err());
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
    assert!(transpile(&format!("x={long_chain};")).is_err());
}
