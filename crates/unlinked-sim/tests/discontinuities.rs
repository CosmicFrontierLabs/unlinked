//! Stateless Discontinuities blocks against closed-form expectations: native
//! Dead Zone, Quantizer and Coulomb & Viscous Friction, and the library links
//! Wrap To Zero, Saturation Dynamic and Dead Zone Dynamic. Formulas follow the
//! MathWorks block references cited on the `Kind` variants.
use std::collections::BTreeMap;
use unlinked_model::validation::DiagnosticTarget;
use unlinked_model::*;
use unlinked_sim::diagnose::{diagnose, DiagnosticContext};
use unlinked_sim::{compile, simulate_model, Options};

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
/// A `Reference` block linked to `source`, with its mask values stored as
/// ordinary parameters the way Simulink saves linked blocks.
fn link(id: &str, source: &str, params: &[(&str, &str)]) -> Block {
    let mut b = block(id, "Reference", params);
    b.library_source = Some(source.into());
    b
}
fn line(source: &str, target: &str, input: u32) -> Line {
    Line {
        src: Some(Endpoint {
            block: source.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
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
        name: "discontinuities".into(),
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
/// A ramp `u = t - 2` over four seconds in exact quarter-second steps feeding
/// the block under test on its first input, with the block's other inputs
/// left to the caller.
fn ramp_through(subject: Block, extra_blocks: Vec<Block>, extra_lines: Vec<Line>) -> Model {
    let mut blocks = vec![
        block("clock", "Clock", &[]),
        block("shift", "Bias", &[("Bias", "-2")]),
        subject,
    ];
    blocks.extend(extra_blocks);
    let mut lines = vec![line("clock", "shift", 1), line("shift", "subject", 1)];
    lines.extend(extra_lines);
    model(blocks, lines)
}
fn ramp_options() -> Options {
    Options {
        stop: 4.0,
        step: 0.25,
        ..Options::default()
    }
}
fn instant() -> Options {
    Options {
        stop: 0.0,
        ..Options::default()
    }
}
fn check(trace: &unlinked_sim::Trace, id: &str, expected: impl Fn(f64) -> f64) {
    assert_eq!(trace.time.len(), 17);
    for (&t, &actual) in trace.time.iter().zip(&trace.signals[id]) {
        let want = expected(t - 2.0);
        assert!(
            (actual - want).abs() < 1e-12,
            "{id} at u={}: {actual} != {want}",
            t - 2.0
        );
    }
}
fn error(model: &Model, options: &Options) -> String {
    match compile(model, options) {
        Ok(graph) => unlinked_sim::simulate(&graph, options)
            .expect_err("expected a rejection")
            .to_string(),
        Err(e) => e.to_string(),
    }
}

#[test]
fn dead_zone_with_asymmetric_bounds_shifts_by_the_exceeded_edge() {
    let m = ramp_through(
        block(
            "subject",
            "DeadZone",
            &[
                ("LowerValue", "-0.5"),
                ("UpperValue", "1"),
                ("ZeroCross", "off"),
            ],
        ),
        vec![],
        vec![],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    check(&t, "subject", |u| {
        if u < -0.5 {
            u + 0.5
        } else if u > 1.0 {
            u - 1.0
        } else {
            0.0
        }
    });
}

#[test]
fn dead_zone_vector_edges_expand_a_scalar_input_and_elementwise_vectors() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "2")]),
            block(
                "subject",
                "DeadZone",
                &[("LowerValue", "-1"), ("UpperValue", "[0.5 1 2.5]")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&m, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![1.5]);
    assert_eq!(t.signals["subject[2]"], vec![1.0]);
    assert_eq!(t.signals["subject[3]"], vec![0.0]);
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "[-3 0 3]")]),
            block(
                "subject",
                "DeadZone",
                &[("LowerValue", "[-1 -1 -1]"), ("UpperValue", "[1 1 2]")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&m, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![-2.0]);
    assert_eq!(t.signals["subject[2]"], vec![0.0]);
    assert_eq!(t.signals["subject[3]"], vec![1.0]);
}

#[test]
fn dead_zone_rejects_crossed_edges_mismatched_widths_and_zero_crossing() {
    let crossed = model(
        vec![
            block("u", "Constant", &[("Value", "2")]),
            block(
                "subject",
                "DeadZone",
                &[("LowerValue", "1"), ("UpperValue", "-1")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    let message = error(&crossed, &instant());
    assert!(
        message.contains("subject") && message.contains("LowerValue"),
        "{message}"
    );
    let mismatched = model(
        vec![
            block("u", "Constant", &[("Value", "[1 2]")]),
            block("subject", "DeadZone", &[("UpperValue", "[1 2 3]")]),
        ],
        vec![line("u", "subject", 1)],
    );
    assert!(error(&mismatched, &instant()).contains("incompatible signal shapes"));
    let zero_cross = model(
        vec![
            block("u", "Constant", &[("Value", "2")]),
            block("subject", "DeadZone", &[("ZeroCross", "on")]),
        ],
        vec![line("u", "subject", 1)],
    );
    let message = error(&zero_cross, &instant());
    assert!(
        message.contains("subject") && message.contains("ZeroCross"),
        "{message}"
    );
    // Exactly the Switch diagnostic.
    let switch = model(
        vec![
            block("u", "Constant", &[("Value", "2")]),
            block(
                "subject",
                "Switch",
                &[("Criteria", "u2 ~= 0"), ("ZeroCross", "on")],
            ),
        ],
        vec![
            line("u", "subject", 1),
            line("u", "subject", 2),
            line("u", "subject", 3),
        ],
    );
    assert_eq!(message, error(&switch, &instant()));
}

#[test]
fn quantizer_ramp_is_a_staircase_with_half_away_from_zero_rounding() {
    let m = ramp_through(
        block("subject", "Quantizer", &[("QuantizationInterval", "0.5")]),
        vec![],
        vec![],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    check(&t, "subject", |u| 0.5 * (u / 0.5).round());
    // Ties at the quarter-second grid: -0.75 / 0.5 = -1.5 rounds to -2.
    let at = |u: f64| t.signals["subject"][((u + 2.0) / 0.25).round() as usize];
    assert_eq!(at(-0.75), -1.0);
    assert_eq!(at(0.75), 1.0);
    assert_eq!(at(-0.25), -0.5);
    let negative = model(
        vec![
            block("u", "Constant", &[("Value", "[-0.3 -0.2 -1.26 0.3]")]),
            block("subject", "Quantizer", &[("QuantizationInterval", "0.5")]),
        ],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&negative, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![-0.5]);
    assert_eq!(t.signals["subject[2]"], vec![0.0]);
    assert_eq!(t.signals["subject[3]"], vec![-1.5]);
    assert_eq!(t.signals["subject[4]"], vec![0.5]);
    let vector_interval = model(
        vec![
            block("u", "Constant", &[("Value", "-1.3")]),
            block(
                "subject",
                "Quantizer",
                &[("QuantizationInterval", "[1 0.25]")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&vector_interval, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![-1.0]);
    assert_eq!(t.signals["subject[2]"], vec![-1.25]);
    for bad in ["0", "-0.5"] {
        let m = model(
            vec![
                block("u", "Constant", &[("Value", "1")]),
                block("subject", "Quantizer", &[("QuantizationInterval", bad)]),
            ],
            vec![line("u", "subject", 1)],
        );
        let message = error(&m, &instant());
        assert!(
            message.contains("subject") && message.contains("QuantizationInterval"),
            "{message}"
        );
    }
}

#[test]
fn coulomb_friction_is_odd_with_an_offset_jump_and_zero_at_rest() {
    let m = ramp_through(
        block(
            "subject",
            "CoulombFriction",
            &[("gain", "2"), ("offset", "1")],
        ),
        vec![],
        vec![],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    check(&t, "subject", |u| {
        if u == 0.0 {
            0.0
        } else {
            u.signum() * (2.0 * u.abs() + 1.0)
        }
    });
    // The library default offset is the vector [1 3 2 0], applied elementwise.
    let vector = model(
        vec![
            block("u", "Constant", &[("Value", "[-1 0 2 -3]")]),
            block("subject", "CoulombFriction", &[]),
        ],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&vector, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![-2.0]);
    assert_eq!(t.signals["subject[2]"], vec![0.0]);
    assert_eq!(t.signals["subject[3]"], vec![4.0]);
    assert_eq!(t.signals["subject[4]"], vec![-3.0]);
    // Simulink saves the block as a library link; both forms agree.
    let linked = model(
        vec![
            block("u", "Constant", &[("Value", "[-1 0 2 -3]")]),
            link(
                "subject",
                "simulink/Discontinuities/Coulomb &\nViscous Friction",
                &[("gain", "[1 1 1 1]"), ("offset", "[1 3 2 0]")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    let linked = simulate_model(&linked, &instant()).unwrap();
    assert_eq!(linked.signals, t.signals);
}

#[test]
fn wrap_to_zero_passes_inputs_up_to_the_threshold_and_zeroes_above() {
    let m = ramp_through(
        link(
            "subject",
            "simulink/Discontinuities/Wrap To Zero",
            &[("Threshold", "1"), ("SourceType", "Wrap To Zero")],
        ),
        vec![],
        vec![],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    check(&t, "subject", |u| if u > 1.0 { 0.0 } else { u });
    let at = |u: f64| t.signals["subject"][((u + 2.0) / 0.25).round() as usize];
    assert_eq!(at(1.0), 1.0, "equal to the threshold passes through");
    assert_eq!(at(1.25), 0.0);
    // Mask-carried parameter values are read when the file keeps the mask.
    let mut masked = link("subject", "simulink/Discontinuities/Wrap To Zero", &[]);
    masked.mask = Some(Mask {
        parameters: vec![MaskParameter {
            name: "Threshold".into(),
            value: "0.5".into(),
            ..MaskParameter::default()
        }],
        ..Mask::default()
    });
    let m = model(
        vec![block("u", "Constant", &[("Value", "[0.5 0.75]")]), masked],
        vec![line("u", "subject", 1)],
    );
    let t = simulate_model(&m, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![0.5]);
    assert_eq!(t.signals["subject[2]"], vec![0.0]);
    // A vector threshold must match the input size exactly.
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "3")]),
            link(
                "subject",
                "simulink/Discontinuities/Wrap To Zero",
                &[("Threshold", "[1 5]")],
            ),
        ],
        vec![line("u", "subject", 1)],
    );
    assert!(error(&m, &instant()).contains("Wrap To Zero requires"));
}

#[test]
fn saturation_dynamic_follows_time_varying_limits_in_up_u_lo_port_order() {
    // up = t, lo = -t, u = 1: y = max(-t, min(1, t)) = min(t, 1).
    let m = model(
        vec![
            block("clock", "Clock", &[]),
            block("neg", "Gain", &[("Gain", "-1")]),
            block("u", "Constant", &[("Value", "1")]),
            link(
                "subject",
                "simulink/Discontinuities/Saturation\nDynamic",
                &[("OutDataTypeStr", "Inherit: Same as second input")],
            ),
        ],
        vec![
            line("clock", "neg", 1),
            line("clock", "subject", 1),
            line("u", "subject", 2),
            line("neg", "subject", 3),
        ],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    for (&time, &y) in t.time.iter().zip(&t.signals["subject"]) {
        assert_eq!(y, time.min(1.0), "at t={time}");
    }
    // Crossed limits: the lower limit wins, as in the library's switch chain.
    let crossed = model(
        vec![
            block("up", "Constant", &[("Value", "-1")]),
            block("u", "Constant", &[("Value", "0")]),
            block("lo", "Constant", &[("Value", "1")]),
            link(
                "subject",
                "simulink/Discontinuities/Saturation Dynamic",
                &[],
            ),
        ],
        vec![
            line("up", "subject", 1),
            line("u", "subject", 2),
            line("lo", "subject", 3),
        ],
    );
    assert_eq!(
        simulate_model(&crossed, &instant()).unwrap().signals["subject"],
        vec![1.0]
    );
    // Vector signal with a scalar upper limit expanded.
    let vector = model(
        vec![
            block("up", "Constant", &[("Value", "0.5")]),
            block("u", "Constant", &[("Value", "[0.2 0.7 -2]")]),
            block("lo", "Constant", &[("Value", "[-1 0.6 -1]")]),
            link(
                "subject",
                "simulink/Discontinuities/Saturation\nDynamic",
                &[],
            ),
        ],
        vec![
            line("up", "subject", 1),
            line("u", "subject", 2),
            line("lo", "subject", 3),
        ],
    );
    let t = simulate_model(&vector, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![0.2]);
    assert_eq!(t.signals["subject[2]"], vec![0.6]);
    assert_eq!(t.signals["subject[3]"], vec![-1.0]);
}

#[test]
fn dead_zone_dynamic_follows_time_varying_limits_and_rejects_crossed_ones() {
    // up = t, lo = -t, u = 1: y = 1 - min(1, t).
    let m = model(
        vec![
            block("clock", "Clock", &[]),
            block("neg", "Gain", &[("Gain", "-1")]),
            block("u", "Constant", &[("Value", "1")]),
            link(
                "subject",
                "simulink/Discontinuities/Dead Zone\nDynamic",
                &[],
            ),
        ],
        vec![
            line("clock", "neg", 1),
            line("clock", "subject", 1),
            line("u", "subject", 2),
            line("neg", "subject", 3),
        ],
    );
    let t = simulate_model(&m, &ramp_options()).unwrap();
    for (&time, &y) in t.time.iter().zip(&t.signals["subject"]) {
        assert_eq!(y, 1.0 - time.min(1.0), "at t={time}");
    }
    // Below the zone: u - lo, with a vector lower limit and scalar others.
    let vector = model(
        vec![
            block("up", "Constant", &[("Value", "1")]),
            block("u", "Constant", &[("Value", "-2")]),
            block("lo", "Constant", &[("Value", "[-1 -3 1]")]),
            link("subject", "simulink/Discontinuities/Dead Zone Dynamic", &[]),
        ],
        vec![
            line("up", "subject", 1),
            line("u", "subject", 2),
            line("lo", "subject", 3),
        ],
    );
    let t = simulate_model(&vector, &instant()).unwrap();
    assert_eq!(t.signals["subject[1]"], vec![-1.0]);
    assert_eq!(t.signals["subject[2]"], vec![0.0]);
    assert_eq!(t.signals["subject[3]"], vec![-3.0]);
    // Limits that cross after one second are an explicit runtime error.
    let crossing = model(
        vec![
            block("clock", "Clock", &[]),
            block("neg", "Gain", &[("Gain", "-1")]),
            block("u", "Constant", &[("Value", "0")]),
            link(
                "subject",
                "simulink/Discontinuities/Dead Zone\nDynamic",
                &[],
            ),
        ],
        vec![
            line("clock", "subject", 3),
            line("u", "subject", 2),
            line("clock", "neg", 1),
            line("neg", "subject", 1),
        ],
    );
    let message = error(&crossing, &ramp_options());
    assert!(
        message.contains("subject") && message.contains("exceeds upper limit"),
        "{message}"
    );
}

#[test]
fn unsupported_links_and_native_names_of_links_still_reject() {
    let backlash = model(
        vec![
            block("u", "Constant", &[("Value", "1")]),
            link("subject", "simulink/Discontinuities/Backlash", &[]),
        ],
        vec![line("u", "subject", 1)],
    );
    assert!(error(&backlash, &instant()).contains("library links"));
    // The link path is what identifies a library block, not a Reference
    // block's display name.
    let unnamed = model(
        vec![
            block("u", "Constant", &[("Value", "1")]),
            block("subject", "Reference", &[("SourceType", "Wrap To Zero")]),
        ],
        vec![line("u", "subject", 1)],
    );
    assert!(error(&unnamed, &instant()).contains("unsupported block type Reference"));
    // Dynamic blocks need all three inputs wired.
    let partial = model(
        vec![
            block("u", "Constant", &[("Value", "1")]),
            link(
                "subject",
                "simulink/Discontinuities/Saturation\nDynamic",
                &[],
            ),
        ],
        vec![line("u", "subject", 2)],
    );
    assert!(error(&partial, &instant()).contains("unconnected input"));
}

#[test]
fn static_diagnostics_accept_supported_links_and_still_flag_others() {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", "1")]),
            link(
                "wrap",
                "simulink/Discontinuities/Wrap To Zero",
                &[("Threshold", "limit")],
            ),
            link("backlash", "simulink/Discontinuities/Backlash", &[]),
        ],
        vec![line("u", "wrap", 1), line("u", "backlash", 1)],
    );
    let report = diagnose(&m, &DiagnosticContext::default());
    let codes: Vec<(String, String)> = report
        .diagnostics
        .iter()
        .filter_map(|d| match &d.target {
            DiagnosticTarget::Block { id, .. } => Some((id.0.clone(), d.code.clone())),
            _ => None,
        })
        .collect();
    assert!(codes.contains(&("backlash".into(), "simulation_link_or_mask".into())));
    assert!(!codes.contains(&("wrap".into(), "simulation_link_or_mask".into())));
    // The link's catalog dialog is consulted for bare workspace names.
    assert!(codes.contains(&("wrap".into(), "workspace_binding_missing".into())));
}
