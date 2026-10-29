//! Analytic checks for the stateless Math Operations blocks: Math Function,
//! Trigonometric Function, Sqrt, Sign, Rounding Function, Unary Minus,
//! Polynomial, Dot Product and MinMax. Each operator is checked against its
//! closed form on scalar and vector inputs at a single sample.
use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::diagnose::{diagnose, CheckMode, DiagnosticContext};
use unlinked_sim::{simulate_model, Options, Trace};

fn block(id: &str, kind: &str, params: &[(&str, &str)]) -> Block {
    Block {
        id: id.into(),
        name: id.into(),
        block_type: kind.into(),
        position: Rect::default(),
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts::default(),
        parameters: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        mask: None,
        library_source: None,
        subsystem: None,
        style: BlockStyle::default(),
        interface: None,
    }
}
fn line(source: &str, output: u32, target: &str, input: u32) -> Line {
    Line {
        src: Some(Endpoint {
            block: source.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: output,
            },
        }),
        dst: Some(Endpoint {
            block: target.into(),
            port: PortRef {
                kind: PortKind::In,
                index: input,
            },
        }),
        ..Line::default()
    }
}
fn model(blocks: Vec<Block>, lines: Vec<Line>) -> Model {
    Model {
        type_defaults: Default::default(),
        charts: Vec::new(),
        name: "math".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        workspace: BTreeMap::new(),
        root: System {
            blocks,
            lines,
            ..System::default()
        },
    }
}
/// A closed-form reference for a one-input operator.
type Unary = fn(f64) -> f64;
fn options() -> Options {
    Options {
        stop: 0.0,
        ..Options::default()
    }
}
/// A `subject` block of `kind` fed by Constant inputs with the given values.
fn subject(kind: &str, params: &[(&str, &str)], inputs: &[&str]) -> Model {
    let mut blocks = vec![block("subject", kind, params)];
    let mut lines = Vec::new();
    for (i, value) in inputs.iter().enumerate() {
        let id = format!("u{}", i + 1);
        blocks.push(block(&id, "Constant", &[("Value", value)]));
        lines.push(line(&id, 1, "subject", i as u32 + 1));
    }
    model(blocks, lines)
}
fn run(kind: &str, params: &[(&str, &str)], inputs: &[&str]) -> Trace {
    simulate_model(&subject(kind, params, inputs), &options())
        .unwrap_or_else(|e| panic!("{kind} {params:?}: {e}"))
}
fn failure(kind: &str, params: &[(&str, &str)], inputs: &[&str]) -> String {
    simulate_model(&subject(kind, params, inputs), &options())
        .err()
        .unwrap_or_else(|| panic!("{kind} {params:?} should reject"))
        .to_string()
}
fn scalar(trace: &Trace, id: &str) -> f64 {
    let values = &trace.signals[id];
    assert_eq!(values.len(), 1);
    values[0]
}
fn vector(trace: &Trace, id: &str, n: usize) -> Vec<f64> {
    (1..=n)
        .map(|k| scalar(trace, &format!("{id}[{k}]")))
        .collect()
}
#[track_caller]
fn close(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            (a - e).abs() <= 1e-12 * e.abs().max(1.0),
            "{actual:?} != {expected:?}"
        );
    }
}
/// Check a one-input elementwise block on a scalar and on a vector.
fn check_unary(kind: &str, params: &[(&str, &str)], inputs: &[f64], f: impl Fn(f64) -> f64) {
    let scalar_input = inputs[0].to_string();
    close(
        &[scalar(&run(kind, params, &[&scalar_input]), "subject")],
        &[f(inputs[0])],
    );
    let literal = format!(
        "[{}]",
        inputs
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let expected: Vec<f64> = inputs.iter().map(|&u| f(u)).collect();
    close(
        &vector(&run(kind, params, &[&literal]), "subject", inputs.len()),
        &expected,
    );
}

#[test]
fn math_function_unary_operators_match_closed_forms() {
    let inputs = [0.5, 2.0, 3.75];
    let cases: &[(&str, Unary)] = &[
        ("exp", f64::exp),
        ("log", f64::ln),
        ("10^u", |u| 10f64.powf(u)),
        ("log10", f64::log10),
        ("magnitude^2", |u| u * u),
        ("square", |u| u * u),
        ("conj", |u| u),
        ("reciprocal", |u| 1.0 / u),
    ];
    for (operator, f) in cases {
        check_unary("Math", &[("Operator", operator)], &inputs, f);
    }
    // magnitude^2 and square agree on negative reals; conj is the identity.
    close(
        &vector(
            &run("Math", &[("Operator", "magnitude^2")], &["[-3 -0.5]"]),
            "subject",
            2,
        ),
        &[9.0, 0.25],
    );
    close(
        &vector(
            &run("Math", &[("Operator", "conj")], &["[-3 -0.5]"]),
            "subject",
            2,
        ),
        &[-3.0, -0.5],
    );
    // Leaving the reals is an explicit error, never a substituted value.
    assert!(failure("Math", &[("Operator", "log")], &["-1"]).contains("non-finite"));
    assert!(failure("Math", &[("Operator", "reciprocal")], &["0"]).contains("non-finite"));
}

#[test]
fn math_function_two_input_operators_expand_scalars() {
    let pow = run("Math", &[("Operator", "pow")], &["[2 3 0.25]", "2"]);
    close(&vector(&pow, "subject", 3), &[4.0, 9.0, 0.0625]);
    let pow = run("Math", &[("Operator", "pow")], &["2", "[1 2 3]"]);
    close(&vector(&pow, "subject", 3), &[2.0, 4.0, 8.0]);
    close(
        &[scalar(
            &run("Math", &[("Operator", "pow")], &["2", "10"]),
            "subject",
        )],
        &[1024.0],
    );
    let hypot = run("Math", &[("Operator", "hypot")], &["[3 5 -8]", "[4 12 6]"]);
    close(&vector(&hypot, "subject", 3), &[5.0, 13.0, 10.0]);
    close(
        &[scalar(
            &run("Math", &[("Operator", "hypot")], &["3", "4"]),
            "subject",
        )],
        &[5.0],
    );
    assert!(
        failure("Math", &[("Operator", "hypot")], &["[1 2]", "[1 2 3]"])
            .contains("incompatible signal shapes")
    );
}

#[test]
fn rem_and_mod_follow_matlab_sign_conventions() {
    // rem takes the sign of the dividend, mod the sign of the divisor
    // (MATLAB rem/mod references).
    let dividends = "[-4 -1 7 9]";
    let rem = run("Math", &[("Operator", "rem")], &[dividends, "3"]);
    close(&vector(&rem, "subject", 4), &[-1.0, -1.0, 1.0, 0.0]);
    let rem = run("Math", &[("Operator", "rem")], &[dividends, "-3"]);
    close(&vector(&rem, "subject", 4), &[-1.0, -1.0, 1.0, 0.0]);
    let modulo = run("Math", &[("Operator", "mod")], &[dividends, "3"]);
    close(&vector(&modulo, "subject", 4), &[2.0, 2.0, 1.0, 0.0]);
    let modulo = run("Math", &[("Operator", "mod")], &[dividends, "-3"]);
    close(&vector(&modulo, "subject", 4), &[-1.0, -1.0, -2.0, 0.0]);
    // Non-integer operands and scalar dividends.
    close(
        &[scalar(
            &run("Math", &[("Operator", "rem")], &["-7.5", "2"]),
            "subject",
        )],
        &[-1.5],
    );
    close(
        &[scalar(
            &run("Math", &[("Operator", "mod")], &["-7.5", "2"]),
            "subject",
        )],
        &[0.5],
    );
    // mod(x, 0) is x; rem(x, 0) is NaN and therefore rejected at runtime.
    close(
        &vector(
            &run("Math", &[("Operator", "mod")], &["[-2 5]", "0"]),
            "subject",
            2,
        ),
        &[-2.0, 5.0],
    );
    assert!(failure("Math", &[("Operator", "rem")], &["5", "0"]).contains("non-finite"));
}

#[test]
fn pow_signed_power_applies_only_to_real_output_type() {
    // Math Function reference: with Signed power on and a real output type,
    // pow is sign(u)*|u|^v; otherwise u^v, which is complex for a negative
    // base with a fractional exponent and so rejects here.
    let signed = run(
        "Math",
        &[("Operator", "pow"), ("OutputSignalType", "real")],
        &["[-8 8 0]", "1/3"],
    );
    close(&vector(&signed, "subject", 3), &[-2.0, 2.0, 0.0]);
    assert!(failure("Math", &[("Operator", "pow")], &["-8", "1/3"]).contains("non-finite"));
    assert!(failure(
        "Math",
        &[
            ("Operator", "pow"),
            ("OutputSignalType", "real"),
            ("SignedPower", "off")
        ],
        &["-8", "1/3"]
    )
    .contains("non-finite"));
    // Integer exponents of negative bases are real either way.
    close(
        &[scalar(
            &run("Math", &[("Operator", "pow")], &["-2", "3"]),
            "subject",
        )],
        &[-8.0],
    );
}

#[test]
fn transpose_and_hermitian_swap_matrix_dimensions() {
    for operator in ["transpose", "hermitian"] {
        let trace = run("Math", &[("Operator", operator)], &["[1 2 3; 4 5 6]"]);
        // Output (r, c) is input (c, r).
        let expected = [
            ("subject[1,1]", 1.0),
            ("subject[1,2]", 4.0),
            ("subject[2,1]", 2.0),
            ("subject[2,2]", 5.0),
            ("subject[3,1]", 3.0),
            ("subject[3,2]", 6.0),
        ];
        for (id, value) in expected {
            assert_eq!(scalar(&trace, id), value, "{operator} {id}");
        }
        assert!(!trace.signals.contains_key("subject[2,3]"));
        // Scalars are unchanged; 1-D vectors have no orientation to swap.
        assert_eq!(
            scalar(&run("Math", &[("Operator", operator)], &["7"]), "subject"),
            7.0
        );
        let error = failure("Math", &[("Operator", operator)], &["[1 2 3]"]);
        assert!(
            error.contains("subject") && error.contains(operator),
            "{error}"
        );
        assert!(error.contains("ambiguous"), "{error}");
    }
    // A transposed column is a row, usable downstream by shape-checked blocks.
    let mut m = subject("Math", &[("Operator", "transpose")], &["[1;2]"]);
    m.root.blocks[1]
        .parameters
        .insert("VectorParams1D".into(), "off".into());
    let trace = simulate_model(&m, &options()).unwrap();
    assert_eq!(scalar(&trace, "subject[1,1]"), 1.0);
    assert_eq!(scalar(&trace, "subject[1,2]"), 2.0);
}

#[test]
fn math_function_rejects_modes_outside_the_subset() {
    for (params, text) in [
        (vec![("Operator", "2^u")], "Operator=2^u"),
        (
            vec![("OutputSignalType", "complex")],
            "OutputSignalType=complex",
        ),
        (
            vec![
                ("Operator", "reciprocal"),
                ("AlgorithmType", "Newton-Raphson"),
            ],
            "AlgorithmType=Newton-Raphson",
        ),
    ] {
        let error = failure("Math", &params, &["2"]);
        assert!(error.contains("subject") && error.contains(text), "{error}");
    }
}

#[test]
fn trigonometric_operators_match_closed_forms() {
    let inputs = [0.25, -0.5, 0.9];
    let cases: &[(&str, Unary)] = &[
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
        ("asinh", f64::asinh),
        ("atanh", f64::atanh),
    ];
    for (operator, f) in cases {
        check_unary("Trigonometry", &[("Operator", operator)], &inputs, f);
    }
    check_unary(
        "Trigonometry",
        &[("Operator", "acosh")],
        &[1.0, 1.5, 4.0],
        f64::acosh,
    );
    assert!(failure("Trigonometry", &[("Operator", "acosh")], &["0.5"]).contains("non-finite"));
    assert!(failure("Trigonometry", &[("Operator", "asin")], &["2"]).contains("non-finite"));
}

#[test]
fn atan2_takes_y_first_and_expands_scalars() {
    let trace = run(
        "Trigonometry",
        &[("Operator", "atan2")],
        &["[1 -1 0]", "-1"],
    );
    close(
        &vector(&trace, "subject", 3),
        &[1f64.atan2(-1.0), (-1f64).atan2(-1.0), 0f64.atan2(-1.0)],
    );
    assert_eq!(
        scalar(
            &run("Trigonometry", &[("Operator", "atan2")], &["1", "1"]),
            "subject"
        ),
        std::f64::consts::FRAC_PI_4
    );
    assert_eq!(
        scalar(
            &run("Trigonometry", &[("Operator", "atan2")], &["1", "0"]),
            "subject"
        ),
        std::f64::consts::FRAC_PI_2
    );
}

#[test]
fn sincos_produces_separate_sine_and_cosine_outputs() {
    let mut m = subject("Trigonometry", &[("Operator", "sincos")], &["[0.3 1.2]"]);
    m.root.blocks[0].ports = PortCounts::from_slice(&[1, 2]);
    m.root.blocks.push(block("sines", "Outport", &[]));
    m.root.blocks.push(block("cosines", "Outport", &[]));
    m.root.lines.push(line("subject", 1, "sines", 1));
    m.root.lines.push(line("subject", 2, "cosines", 1));
    let trace = simulate_model(&m, &options()).unwrap();
    close(
        &vector(&trace, "subject:out:1", 2),
        &[0.3f64.sin(), 1.2f64.sin()],
    );
    close(
        &vector(&trace, "subject:out:2", 2),
        &[0.3f64.cos(), 1.2f64.cos()],
    );
    close(&vector(&trace, "sines", 2), &[0.3f64.sin(), 1.2f64.sin()]);
    close(&vector(&trace, "cosines", 2), &[0.3f64.cos(), 1.2f64.cos()]);
    let mut m = subject("Trigonometry", &[("Operator", "sincos")], &["0.3"]);
    m.root.blocks[0].ports = PortCounts::from_slice(&[1, 2]);
    let trace = simulate_model(&m, &options()).unwrap();
    assert_eq!(scalar(&trace, "subject:out:1"), 0.3f64.sin());
    assert_eq!(scalar(&trace, "subject:out:2"), 0.3f64.cos());
}

#[test]
fn trigonometry_rejects_complex_output_and_approximations() {
    for (params, text) in [
        (vec![("Operator", "cos + jsin")], "Operator=cos + jsin"),
        (
            vec![("ApproximationMethod", "CORDIC")],
            "ApproximationMethod=CORDIC",
        ),
        (
            vec![("ApproximationMethod", "Lookup")],
            "ApproximationMethod=Lookup",
        ),
        (
            vec![("OutputSignalType", "complex")],
            "OutputSignalType=complex",
        ),
    ] {
        let error = failure("Trigonometry", &params, &["2"]);
        assert!(error.contains("subject") && error.contains(text), "{error}");
    }
    // The same restriction is reported statically against the parameter.
    let report = diagnose(
        &subject("Trigonometry", &[("Operator", "cos + jsin")], &["2"]),
        &DiagnosticContext {
            mode: CheckMode::Static,
            ..Default::default()
        },
    );
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "simulation_mode"
            && matches!(
                &d.target,
                unlinked_model::validation::DiagnosticTarget::Block { id, parameter, .. }
                    if id.0 == "subject" && parameter.as_deref() == Some("Operator")
            )));
}

#[test]
fn sqrt_variants_match_definitions() {
    check_unary("Sqrt", &[], &[4.0, 0.25, 9.0], f64::sqrt);
    check_unary(
        "Sqrt",
        &[("Operator", "sqrt")],
        &[4.0, 0.25, 9.0],
        f64::sqrt,
    );
    // signedSqrt is sign(u)*sqrt(|u|), defined for negatives.
    check_unary(
        "Sqrt",
        &[("Operator", "signedSqrt")],
        &[-4.0, 9.0, 0.0, -0.25],
        |u| {
            if u == 0.0 {
                0.0
            } else {
                u.signum() * u.abs().sqrt()
            }
        },
    );
    check_unary("Sqrt", &[("Operator", "rSqrt")], &[4.0, 0.25, 16.0], |u| {
        1.0 / u.sqrt()
    });
    assert!(failure("Sqrt", &[], &["-4"]).contains("non-finite"));
    assert!(failure("Sqrt", &[("Operator", "rSqrt")], &["0"]).contains("non-finite"));
    for (params, text) in [
        (
            vec![("OutputSignalType", "complex")],
            "OutputSignalType=complex",
        ),
        (
            vec![
                ("Operator", "rSqrt"),
                ("RSqrtAlgorithmType", "Newton-Raphson"),
            ],
            "RSqrtAlgorithmType=Newton-Raphson",
        ),
        (
            vec![("SqrtAlgorithmType", "Lookup")],
            "SqrtAlgorithmType=Lookup",
        ),
    ] {
        let error = failure("Sqrt", &params, &["4"]);
        assert!(error.contains("subject") && error.contains(text), "{error}");
    }
}

#[test]
fn signum_rounding_and_unary_minus() {
    check_unary("Signum", &[], &[-3.0, 0.0, 2.5, -0.0], |u| {
        if u == 0.0 {
            0.0
        } else {
            u.signum()
        }
    });
    let inputs = [2.5, -2.5, -1.2, 1.7, 3.0];
    check_unary("Rounding", &[], &inputs, f64::floor);
    check_unary("Rounding", &[("Operator", "floor")], &inputs, f64::floor);
    check_unary("Rounding", &[("Operator", "ceil")], &inputs, f64::ceil);
    check_unary("Rounding", &[("Operator", "fix")], &inputs, f64::trunc);
    // MATLAB round: halves away from zero.
    let rounded = run(
        "Rounding",
        &[("Operator", "round")],
        &["[2.5 -2.5 0.5 -0.5 1.49]"],
    );
    close(
        &vector(&rounded, "subject", 5),
        &[3.0, -3.0, 1.0, -1.0, 1.0],
    );
    let error = failure("Rounding", &[("Operator", "nearest")], &["2"]);
    assert!(
        error.contains("subject") && error.contains("Operator=nearest"),
        "{error}"
    );
    check_unary("UnaryMinus", &[], &[1.0, -2.5, 0.0], |u| -u);
}

#[test]
fn polynomial_evaluates_descending_coefficients_elementwise() {
    check_unary("Polyval", &[("coefs", "[2 -3 1]")], &[0.0, 1.0, 2.5], |u| {
        2.0 * u * u - 3.0 * u + 1.0
    });
    check_unary("Polyval", &[("coefs", "7")], &[0.0, 1.0], |_| 7.0);
    check_unary("Polyval", &[("coefs", "[1 0 0 0]")], &[-2.0, 3.0], |u| {
        u * u * u
    });
    let mut m = subject("Polyval", &[("coefs", "k")], &["2"]);
    m.workspace.insert("k".into(), "[1 1]".into());
    assert_eq!(
        scalar(&simulate_model(&m, &options()).unwrap(), "subject"),
        3.0
    );
    let error = failure("Polyval", &[("coefs", "[1 2; 3 4]")], &["2"]);
    assert!(
        error.contains("subject") && error.contains("coefs"),
        "{error}"
    );
}

#[test]
fn dot_product_reduces_vectors_to_a_scalar() {
    assert_eq!(
        scalar(&run("DotProduct", &[], &["[1 2 3]", "[4 5 6]"]), "subject"),
        32.0
    );
    assert_eq!(
        scalar(&run("DotProduct", &[], &["[1;2;3]", "[4;5;6]"]), "subject"),
        32.0
    );
    assert_eq!(
        scalar(&run("DotProduct", &[], &["3", "4"]), "subject"),
        12.0
    );
    // A scalar expands against a vector, as sum(u1 .* u2) does.
    let trace = run("DotProduct", &[], &["2", "[1 2 3]"]);
    assert_eq!(scalar(&trace, "subject"), 12.0);
    assert_eq!(scalar(&trace, "subject:$dot:3"), 6.0);
    let error = failure("DotProduct", &[], &["[1 2]", "[1 2 3]"]);
    assert!(
        error.contains("subject") && error.contains("equal lengths"),
        "{error}"
    );
    let error = failure("DotProduct", &[], &["[1 2; 3 4]", "[1 2; 3 4]"]);
    assert!(error.contains("column vectors"), "{error}");
}

#[test]
fn minmax_reduces_one_input_and_works_elementwise_across_inputs() {
    // Inputs=1 (the Simulink default) reduces a vector to a scalar.
    assert_eq!(scalar(&run("MinMax", &[], &["[3 -1 7]"]), "subject"), -1.0);
    assert_eq!(
        scalar(
            &run("MinMax", &[("Function", "max")], &["[3 -1 7]"]),
            "subject"
        ),
        7.0
    );
    assert_eq!(scalar(&run("MinMax", &[], &["4"]), "subject"), 4.0);
    // Several inputs compare elementwise with scalar expansion.
    let min = run("MinMax", &[("Inputs", "2")], &["[1 5 3]", "4"]);
    close(&vector(&min, "subject", 3), &[1.0, 4.0, 3.0]);
    let max = run(
        "MinMax",
        &[("Function", "max"), ("Inputs", "2")],
        &["[1 5 3]", "4"],
    );
    close(&vector(&max, "subject", 3), &[4.0, 5.0, 4.0]);
    let three = run(
        "MinMax",
        &[("Function", "max"), ("Inputs", "3")],
        &["[1 9]", "[8 2]", "-5"],
    );
    close(&vector(&three, "subject", 2), &[8.0, 9.0]);
    assert_eq!(
        scalar(
            &run("MinMax", &[("Inputs", "3")], &["2", "-7", "5"]),
            "subject"
        ),
        -7.0
    );
    let error = failure("MinMax", &[], &["[1 2; 3 4]"]);
    assert!(error.contains("one-dimensional vector"), "{error}");
    let error = failure(
        "MinMax",
        &[("CollapseMode", "Specified dimension")],
        &["[1 2 3]"],
    );
    assert!(
        error.contains("CollapseMode=Specified dimension"),
        "{error}"
    );
    let error = failure("MinMax", &[("Function", "median")], &["[1 2 3]"]);
    assert!(error.contains("Function=median"), "{error}");
    let error = failure("MinMax", &[("Inputs", "2")], &["[1 2]", "[1 2 3]"]);
    assert!(error.contains("incompatible signal shapes"), "{error}");
}

#[test]
fn running_minmax_library_block_is_rejected_explicitly() {
    let mut m = subject(
        "Reference",
        &[("SourceType", "MinMax Running Resettable")],
        &["1", "0"],
    );
    m.root.blocks[0].library_source =
        Some("simulink/Math\nOperations/MinMax\nRunning\nResettable".into());
    let error = simulate_model(&m, &options()).unwrap_err().to_string();
    // Library-sourced Reference blocks are rejected as a whole; no running
    // min/max or reset-port semantics are approximated.
    assert!(
        error.contains("subject") && error.contains("unsupported block type Reference"),
        "{error}"
    );
}
