//! Commented blocks: Simulink's Comment Out (`Commented=on`) removes a block,
//! terminating its inputs and grounding its destinations; Comment Through
//! (`Commented=through`) removes it and wires input i to output i.
use std::collections::BTreeMap;
use unlinked_model::validation::{DiagnosticTarget, Severity};
use unlinked_model::*;
use unlinked_sim::diagnose::{diagnose, DiagnosticContext};
use unlinked_sim::{compile, simulate_model, Options, Trace};

fn block(id: &str, kind: &str, params: &[(&str, &str)], inputs: u32, outputs: u32) -> Block {
    Block {
        id: id.into(),
        name: id.into(),
        block_type: kind.into(),
        position: Rect::default(),
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts {
            inputs,
            outputs,
            ..PortCounts::default()
        },
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
        name: "commented".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        type_defaults: Default::default(),
        charts: Vec::new(),
        workspace: BTreeMap::new(),
        root: System {
            blocks,
            lines,
            ..System::default()
        },
    }
}
fn options() -> Options {
    Options {
        stop: 1.0,
        step: 0.1,
        ..Options::default()
    }
}
fn run(m: &Model) -> Trace {
    simulate_model(m, &options()).unwrap()
}
fn error(m: &Model) -> String {
    compile(m, &options()).unwrap_err().to_string()
}
/// `signal` equals `expected(t)` at every sample.
fn assert_signal(trace: &Trace, signal: &str, expected: impl Fn(f64) -> f64) {
    let values = &trace.signals[signal];
    assert_eq!(values.len(), trace.time.len());
    for (t, v) in trace.time.iter().zip(values) {
        assert!(
            (v - expected(*t)).abs() < 1e-12,
            "{signal} at t={t}: {v} != {}",
            expected(*t)
        );
    }
}
fn with(params: &[(&str, &str)], extra: &[(&str, &str)]) -> Vec<(&'static str, &'static str)> {
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_owned().into_boxed_str())
    }
    params
        .iter()
        .chain(extra)
        .map(|(k, v)| (leak(k), leak(v)))
        .collect()
}

#[test]
fn commented_out_scope_and_commented_through_gain_match_the_edited_model() {
    let clock = || block("clock", "Clock", &[], 0, 1);
    let one = || block("one", "Constant", &[("Value", "1")], 0, 1);
    let sum = || block("sum", "Sum", &[("Inputs", "++")], 2, 1);
    let commented = model(
        vec![
            clock(),
            block(
                "gain",
                "Gain",
                &[("Gain", "2"), ("Commented", "through")],
                1,
                1,
            ),
            one(),
            sum(),
            block("scope", "Scope", &[("Commented", "on")], 1, 0),
        ],
        vec![
            line("clock", 1, "gain", 1),
            line("gain", 1, "sum", 1),
            line("one", 1, "sum", 2),
            line("gain", 1, "scope", 1),
        ],
    );
    let edited = model(
        vec![clock(), one(), sum()],
        vec![line("clock", 1, "sum", 1), line("one", 1, "sum", 2)],
    );
    let (a, b) = (run(&commented), run(&edited));
    assert_eq!(a.time, b.time);
    assert_eq!(
        a.signals, b.signals,
        "removed blocks leave no trace of their own"
    );
    assert_signal(&a, "sum", |t| t + 1.0);

    // Uncommented, the gain doubles the clock and the scope records it.
    let mut live = commented.clone();
    live.root.blocks[1]
        .parameters
        .insert("Commented".into(), "off".into());
    live.root.blocks[4].parameters.remove("Commented");
    let t = run(&live);
    assert_signal(&t, "sum", |t| 2.0 * t + 1.0);
    assert_signal(&t, "gain", |t| 2.0 * t);
    assert!(t.signals.contains_key("scope"));
}

#[test]
fn commented_out_block_feeding_a_sum_reads_zero_and_never_evaluates_its_parameters() {
    let m = model(
        vec![
            block("clock", "Clock", &[], 0, 1),
            block(
                "gain",
                "Gain",
                &[("Gain", "not a number"), ("Commented", "on")],
                1,
                1,
            ),
            block("three", "Constant", &[("Value", "3")], 0, 1),
            block("sum", "Sum", &[("Inputs", "++")], 2, 1),
        ],
        vec![
            line("clock", 1, "gain", 1),
            line("gain", 1, "sum", 1),
            line("three", 1, "sum", 2),
        ],
    );
    let t = run(&m);
    assert_signal(&t, "sum", |_| 3.0);
    // The grounded output is recorded under the commented block's ID, and the
    // terminated clock still runs.
    assert_signal(&t, "gain", |_| 0.0);
    assert_signal(&t, "clock", |t| t);

    let mut live = m.clone();
    live.root.blocks[1].parameters.remove("Commented");
    assert!(error(&live).contains("gain"));
}

#[test]
fn vector_signals_pass_through_and_ground() {
    let mut m = model(
        vec![
            block("a", "Constant", &[("Value", "[1 2 3]")], 0, 1),
            block(
                "gain",
                "Gain",
                &[("Gain", "10"), ("Commented", "through")],
                1,
                1,
            ),
            block("b", "Constant", &[("Value", "[4 5 6]")], 0, 1),
            block("sum", "Sum", &[("Inputs", "++")], 2, 1),
        ],
        vec![
            line("a", 1, "gain", 1),
            line("gain", 1, "sum", 1),
            line("b", 1, "sum", 2),
        ],
    );
    let t = run(&m);
    for (i, expected) in [5.0, 7.0, 9.0].into_iter().enumerate() {
        assert_signal(&t, &format!("sum[{}]", i + 1), |_| expected);
    }
    // Commented out, the vector operand becomes a scalar ground that
    // broadcasts over the other operand.
    m.root.blocks[1]
        .parameters
        .insert("Commented".into(), "on".into());
    let t = run(&m);
    for (i, expected) in [4.0, 5.0, 6.0].into_iter().enumerate() {
        assert_signal(&t, &format!("sum[{}]", i + 1), |_| expected);
    }
}

#[test]
fn nested_subsystems_honour_commented_blocks() {
    let inner = |gain_params: &[(&str, &str)]| System {
        blocks: vec![
            block("in", "Inport", &[("Port", "1")], 0, 1),
            block("g", "Gain", gain_params, 1, 1),
            block("out", "Outport", &[("Port", "1")], 1, 0),
        ],
        lines: vec![line("in", 1, "g", 1), line("g", 1, "out", 1)],
        ..System::default()
    };
    let sub = |params: &[(&str, &str)], system: System| {
        let mut b = block("sub", "SubSystem", params, 1, 1);
        b.subsystem = Some(Box::new(system));
        b
    };
    let root = |sub: Block| {
        model(
            vec![
                block("clock", "Clock", &[], 0, 1),
                sub,
                block("two", "Constant", &[("Value", "2")], 0, 1),
                block("sum", "Sum", &[("Inputs", "++")], 2, 1),
            ],
            vec![
                line("clock", 1, "sub", 1),
                line("sub", 1, "sum", 1),
                line("two", 1, "sum", 2),
            ],
        )
    };
    let gain = [("Gain", "7")];
    let t = run(&root(sub(&[], inner(&gain))));
    assert_signal(&t, "sum", |t| 7.0 * t + 2.0);
    // A commented-through gain inside the subsystem is an identity.
    let t = run(&root(sub(
        &[],
        inner(&with(&gain, &[("Commented", "through")])),
    )));
    assert_signal(&t, "sum", |t| t + 2.0);
    // A commented-out gain grounds the subsystem's Outport.
    let t = run(&root(sub(&[], inner(&with(&gain, &[("Commented", "on")])))));
    assert_signal(&t, "sum", |_| 2.0);
    // The subsystem itself can be commented through or out.
    let t = run(&root(sub(&[("Commented", "through")], inner(&gain))));
    assert_signal(&t, "sum", |t| t + 2.0);
    let t = run(&root(sub(&[("Commented", "on")], inner(&gain))));
    assert_signal(&t, "sum", |_| 2.0);
    assert_signal(&t, "sub", |_| 0.0);
}

#[test]
fn commented_blocks_around_goto_from_routing() {
    let routed = |gain_params: &[(&str, &str)], goto_params: &[(&str, &str)]| {
        model(
            vec![
                block("clock", "Clock", &[], 0, 1),
                block("gain", "Gain", gain_params, 1, 1),
                block("goto", "Goto", goto_params, 1, 0),
                block("from", "From", &[("GotoTag", "A")], 0, 1),
                block("two", "Constant", &[("Value", "2")], 0, 1),
                block("sum", "Sum", &[("Inputs", "++")], 2, 1),
            ],
            vec![
                line("clock", 1, "gain", 1),
                line("gain", 1, "goto", 1),
                line("from", 1, "sum", 1),
                line("two", 1, "sum", 2),
            ],
        )
    };
    let gain = [("Gain", "5")];
    let goto = [("GotoTag", "A")];
    let t = run(&routed(&gain, &goto));
    assert_signal(&t, "sum", |t| 5.0 * t + 2.0);
    // The block driving the Goto is commented out: the From reads ground.
    let t = run(&routed(&with(&gain, &[("Commented", "on")]), &goto));
    assert_signal(&t, "sum", |_| 2.0);
    // Commented through, it passes the clock to the Goto.
    let t = run(&routed(&with(&gain, &[("Commented", "through")]), &goto));
    assert_signal(&t, "sum", |t| t + 2.0);
    // A commented-out Goto leaves its From grounded.
    let t = run(&routed(&gain, &with(&goto, &[("Commented", "on")])));
    assert_signal(&t, "sum", |_| 2.0);
    // Routing blocks have no paired ports to comment through.
    let e = error(&routed(&gain, &with(&goto, &[("Commented", "through")])));
    assert!(
        e.contains("block goto") && e.contains("Commented=through"),
        "{e}"
    );
}

#[test]
fn contents_of_a_commented_subsystem_are_never_routed_or_validated() {
    // The body would fail routing live: a From with no Goto drives the
    // Outport and a Goto has scoped visibility. Simulink ignores everything
    // inside a commented block, so compile and diagnose must agree it is fine.
    let body = || System {
        blocks: vec![
            block("in", "Inport", &[("Port", "1")], 0, 1),
            block(
                "goto",
                "Goto",
                &[("GotoTag", "S"), ("TagVisibility", "scoped")],
                1,
                0,
            ),
            block("from", "From", &[("GotoTag", "MISSING")], 0, 1),
            block("out", "Outport", &[("Port", "1")], 1, 0),
        ],
        lines: vec![line("in", 1, "goto", 1), line("from", 1, "out", 1)],
        ..System::default()
    };
    let root = |params: &[(&str, &str)]| {
        let mut sub = block("sub", "SubSystem", params, 1, 1);
        sub.subsystem = Some(Box::new(body()));
        model(
            vec![
                block("clock", "Clock", &[], 0, 1),
                sub,
                block("two", "Constant", &[("Value", "2")], 0, 1),
                block("sum", "Sum", &[("Inputs", "++")], 2, 1),
            ],
            vec![
                line("clock", 1, "sub", 1),
                line("sub", 1, "sum", 1),
                line("two", 1, "sum", 2),
            ],
        )
    };
    let e = error(&root(&[]));
    assert!(e.contains("Goto"), "{e}");
    for (setting, expected) in [("on", 0.0), ("through", 1.0)] {
        let m = root(&[("Commented", setting)]);
        assert_signal(&run(&m), "sum", |t| expected * t + 2.0);
        let report = diagnose(&m, &DiagnosticContext::default());
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error && d.code.starts_with("simulation_")),
            "{setting}: {:?}",
            report.diagnostics
        );
    }
    // Nested deeper, the commented level still shields everything below it.
    let mut outer = block("outer", "SubSystem", &[("Commented", "on")], 1, 1);
    let mut inner = block("sub", "SubSystem", &[], 1, 1);
    inner.subsystem = Some(Box::new(body()));
    outer.subsystem = Some(Box::new(System {
        blocks: vec![
            block("in", "Inport", &[("Port", "1")], 0, 1),
            inner,
            block("out", "Outport", &[("Port", "1")], 1, 0),
        ],
        lines: vec![line("in", 1, "sub", 1), line("sub", 1, "out", 1)],
        ..System::default()
    }));
    let mut m = root(&[]);
    m.root.blocks[1] = outer;
    m.root.lines = vec![
        line("clock", 1, "outer", 1),
        line("outer", 1, "sum", 1),
        line("two", 1, "sum", 2),
    ];
    assert_signal(&run(&m), "sum", |_| 2.0);
}

#[test]
fn commented_out_routing_blocks_need_no_match_and_are_never_validated() {
    let routed = |goto_params: &[(&str, &str)], from_params: &[(&str, &str)]| {
        model(
            vec![
                block("clock", "Clock", &[], 0, 1),
                block("goto", "Goto", goto_params, 1, 0),
                block("from", "From", from_params, 0, 1),
                block("two", "Constant", &[("Value", "2")], 0, 1),
                block("sum", "Sum", &[("Inputs", "++")], 2, 1),
            ],
            vec![
                line("clock", 1, "goto", 1),
                line("from", 1, "sum", 1),
                line("two", 1, "sum", 2),
            ],
        )
    };
    // A commented-out scoped Goto is not refused; its From reads ground.
    let scoped = [
        ("GotoTag", "A"),
        ("TagVisibility", "scoped"),
        ("Commented", "on"),
    ];
    assert!(error(&routed(&scoped[..2], &[("GotoTag", "A")])).contains("visibility"));
    assert_signal(&run(&routed(&scoped, &[("GotoTag", "A")])), "sum", |_| 2.0);
    // A commented-out From with no matching Goto is simply gone.
    let t = run(&routed(
        &[("GotoTag", "A")],
        &[("GotoTag", "NOWHERE"), ("Commented", "on")],
    ));
    assert_signal(&t, "sum", |_| 2.0);
    // A live Goto keeps its tag even when a commented-out one shares it.
    let mut m = routed(&[("GotoTag", "A")], &[("GotoTag", "A")]);
    m.root.blocks.push(block(
        "goto2",
        "Goto",
        &[("GotoTag", "A"), ("Commented", "on")],
        1,
        0,
    ));
    m.root.lines.push(line("two", 1, "goto2", 1));
    assert_signal(&run(&m), "sum", |t| t + 2.0);
}

#[test]
fn comment_through_pairs_input_i_with_output_i() {
    // The subsystem swaps its ports; commented through, the swap is gone and
    // input i feeds output i as in Simulink.
    let mut swap = block("swap", "SubSystem", &[("Commented", "through")], 2, 2);
    swap.subsystem = Some(Box::new(System {
        blocks: vec![
            block("in1", "Inport", &[("Port", "1")], 0, 1),
            block("in2", "Inport", &[("Port", "2")], 0, 1),
            block("out1", "Outport", &[("Port", "1")], 1, 0),
            block("out2", "Outport", &[("Port", "2")], 1, 0),
        ],
        lines: vec![line("in1", 1, "out2", 1), line("in2", 1, "out1", 1)],
        ..System::default()
    }));
    let m = model(
        vec![
            block("clock", "Clock", &[], 0, 1),
            block("ten", "Constant", &[("Value", "10")], 0, 1),
            swap,
            block("a", "Gain", &[("Gain", "1")], 1, 1),
            block("b", "Gain", &[("Gain", "1")], 1, 1),
        ],
        vec![
            line("clock", 1, "swap", 1),
            line("ten", 1, "swap", 2),
            line("swap", 1, "a", 1),
            line("swap", 2, "b", 1),
        ],
    );
    let t = run(&m);
    assert_signal(&t, "a", |t| t);
    assert_signal(&t, "b", |_| 10.0);
    // Live, the swap is in effect.
    let mut live = m.clone();
    live.root.blocks[2].parameters.remove("Commented");
    let t = run(&live);
    assert_signal(&t, "a", |_| 10.0);
    assert_signal(&t, "b", |t| t);
}

#[test]
fn conditional_subsystem_without_a_ports_vector_is_refused_comment_through() {
    // Hand-built blocks may carry no `Ports` vector; the Enable port block
    // inside still makes this a conditional subsystem.
    let mut sub = block("sub", "SubSystem", &[("Commented", "through")], 0, 0);
    sub.subsystem = Some(Box::new(System {
        blocks: vec![
            block("in", "Inport", &[("Port", "1")], 0, 1),
            block("en", "EnablePort", &[], 0, 0),
            block("out", "Outport", &[("Port", "1")], 1, 0),
        ],
        lines: vec![line("in", 1, "out", 1)],
        ..System::default()
    }));
    let m = model(
        vec![
            block("one", "Constant", &[("Value", "1")], 0, 1),
            sub,
            block("sink", "Scope", &[], 1, 0),
        ],
        vec![line("one", 1, "sub", 1), line("sub", 1, "sink", 1)],
    );
    let e = error(&m);
    assert!(
        e.contains("block sub") && e.contains("control or connection ports"),
        "{e}"
    );
}

#[test]
fn comment_through_is_refused_when_ports_do_not_pair_up() {
    let m = |sum_params: &[(&str, &str)], sink_params: &[(&str, &str)]| {
        model(
            vec![
                block("c", "Constant", &[("Value", "1")], 0, 1),
                block("sum", "Sum", sum_params, 2, 1),
                block("sink", "Scope", sink_params, 1, 0),
            ],
            vec![
                line("c", 1, "sum", 1),
                line("c", 1, "sum", 2),
                line("sum", 1, "sink", 1),
            ],
        )
    };
    let sum = [("Inputs", "++")];
    let e = error(&m(&with(&sum, &[("Commented", "through")]), &[]));
    assert!(
        e.contains("block sum") && e.contains("2 inputs and 1 outputs"),
        "{e}"
    );
    let e = error(&m(&sum, &[("Commented", "through")]));
    assert!(
        e.contains("block sink") && e.contains("1 inputs and 0 outputs"),
        "{e}"
    );
    // Unknown values are refused, never ignored.
    let e = error(&m(&with(&sum, &[("Commented", "maybe")]), &[]));
    assert!(
        e.contains("block sum") && e.contains("Commented=maybe"),
        "{e}"
    );
    // Simulink never comments ports.
    let mut port = m(&sum, &[]);
    port.root.blocks.push(block(
        "in",
        "Inport",
        &[("Port", "1"), ("Commented", "on")],
        0,
        1,
    ));
    assert!(error(&port).contains("cannot be commented"));
}

#[test]
fn undriven_and_chained_commented_blocks_resolve_to_their_real_source() {
    let tail = || {
        vec![
            block("one", "Constant", &[("Value", "1")], 0, 1),
            block("sum", "Sum", &[("Inputs", "++")], 2, 1),
        ]
    };
    // A commented-through block with no input grounds its destination
    // instead of reporting a missing driver.
    let mut blocks = vec![block(
        "gain",
        "Gain",
        &[("Gain", "3"), ("Commented", "through")],
        1,
        1,
    )];
    blocks.extend(tail());
    let t = run(&model(
        blocks,
        vec![line("gain", 1, "sum", 1), line("one", 1, "sum", 2)],
    ));
    assert_signal(&t, "sum", |_| 1.0);
    assert!(
        t.signals.keys().any(|k| k == "gain.commented.in1"),
        "{:?}",
        t.signals.keys().collect::<Vec<_>>()
    );

    let chain = |first: &str, second: &str| {
        let mut blocks = vec![
            block("clock", "Clock", &[], 0, 1),
            block("g1", "Gain", &[("Gain", "3"), ("Commented", first)], 1, 1),
            block("g2", "Gain", &[("Gain", "5"), ("Commented", second)], 1, 1),
        ];
        blocks.extend(tail());
        model(
            blocks,
            vec![
                line("clock", 1, "g1", 1),
                line("g1", 1, "g2", 1),
                line("g2", 1, "sum", 1),
                line("one", 1, "sum", 2),
            ],
        )
    };
    assert_signal(&run(&chain("off", "off")), "sum", |t| 15.0 * t + 1.0);
    assert_signal(&run(&chain("through", "through")), "sum", |t| t + 1.0);
    assert_signal(&run(&chain("on", "through")), "sum", |_| 1.0);
    assert_signal(&run(&chain("through", "on")), "sum", |_| 1.0);
    assert_signal(&run(&chain("through", "off")), "sum", |t| 5.0 * t + 1.0);
}

#[test]
fn static_diagnostics_skip_commented_parameters_but_report_port_mismatch() {
    let m = model(
        vec![
            block("c", "Constant", &[("Value", "1")], 0, 1),
            block("gain", "Gain", &[("Gain", "K"), ("Commented", "on")], 1, 1),
            block(
                "sum",
                "Sum",
                &[("Inputs", "++"), ("Commented", "through")],
                2,
                1,
            ),
            block(
                "weird",
                "Gain",
                &[("Gain", "1"), ("Commented", "maybe")],
                1,
                1,
            ),
        ],
        vec![
            line("c", 1, "gain", 1),
            line("gain", 1, "sum", 1),
            line("c", 1, "sum", 2),
            line("sum", 1, "weird", 1),
        ],
    );
    let report = diagnose(&m, &DiagnosticContext::default());
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|d| d.code == "workspace_binding_missing"),
        "{:?}",
        report.diagnostics
    );
    let refused = |id: &str, text: &str| {
        report.diagnostics.iter().any(|d| {
            d.code == "simulation_commented"
                && d.severity == Severity::Error
                && d.message.contains(text)
                && matches!(&d.target, DiagnosticTarget::Block { id: target, parameter: Some(p), .. }
                    if target.0 == id && p == "Commented")
        })
    };
    assert!(
        refused("sum", "2 inputs and 1 outputs"),
        "{:?}",
        report.diagnostics
    );
    assert!(
        refused("weird", "Commented=maybe"),
        "{:?}",
        report.diagnostics
    );
    // Uncommented, the unresolved gain parameter is reported again.
    let mut live = m.clone();
    live.root.blocks[1].parameters.remove("Commented");
    assert!(diagnose(&live, &DiagnosticContext::default())
        .diagnostics
        .iter()
        .any(|d| d.code == "workspace_binding_missing"));
}
