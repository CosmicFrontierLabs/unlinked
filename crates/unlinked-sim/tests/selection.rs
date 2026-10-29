//! Signal Routing selection blocks: Selector, Multiport Switch (and the
//! library Index Vector), Manual Switch, Environment Controller and the Switch
//! threshold criteria. Expected values are closed-form.
use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::diagnose::{diagnose, DiagnosticContext};
use unlinked_sim::{compile, simulate_model, Options, Solver};

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
fn library(id: &str, source: &str, inputs: u32, params: &[(&str, &str)]) -> Block {
    let mut b = block(id, "Reference", params);
    b.library_source = Some(source.into());
    b.ports = PortCounts::from_slice(&[inputs, 1]);
    b
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
        name: "selection".into(),
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
fn once() -> Options {
    Options {
        stop: 0.0,
        ..Options::default()
    }
}
fn error(m: &Model) -> String {
    compile(m, &once()).unwrap_err().to_string()
}
/// `source -> Selector(params) -> o` evaluated once.
fn select(value: &str, params: &[(&str, &str)]) -> Result<BTreeMap<String, Vec<f64>>, String> {
    let m = model(
        vec![
            block("u", "Constant", &[("Value", value)]),
            block("sel", "Selector", params),
            block("o", "Outport", &[]),
        ],
        vec![line("u", 1, "sel", 1), line("sel", 1, "o", 1)],
    );
    simulate_model(&m, &once())
        .map(|t| t.signals)
        .map_err(|e| e.to_string())
}

#[test]
fn selector_picks_three_then_one_from_a_vector() {
    let s = select(
        "[10 20 30]",
        &[
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("IndexParamArray", "{'[3 1]'}"),
        ],
    )
    .unwrap();
    assert_eq!(s["o[1]"], vec![30.0]);
    assert_eq!(s["o[2]"], vec![10.0]);
    assert!(!s.contains_key("o[3]"));
    // Zero-based indexing names the same elements one lower.
    let s = select(
        "[10 20 30]",
        &[
            ("IndexMode", "Zero-based"),
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("IndexParamArray", "{'[2 0]'}"),
        ],
    )
    .unwrap();
    assert_eq!((s["o[1]"][0], s["o[2]"][0]), (30.0, 10.0));
    // A single selected element is a scalar signal.
    let s = select(
        "[10 20 30]",
        &[
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("IndexParamArray", "{'2'}"),
        ],
    )
    .unwrap();
    assert_eq!(s["o"], vec![20.0]);
    // Starting index with an output size selects a contiguous range.
    let s = select(
        "[10 20 30 40]",
        &[
            ("IndexOptionArray", "{'Starting index (dialog)'}"),
            ("IndexParamArray", "{'2'}"),
            ("OutputSizeArray", "{'3'}"),
        ],
    )
    .unwrap();
    assert_eq!(
        (s["o[1]"][0], s["o[2]"][0], s["o[3]"][0]),
        (20.0, 30.0, 40.0)
    );
    // Select all passes the vector through.
    let s = select("[10 20]", &[("IndexOptionArray", "{'Select all'}")]).unwrap();
    assert_eq!((s["o[1]"][0], s["o[2]"][0]), (10.0, 20.0));
    // Older comma-separated spellings resolve the same way.
    let s = select(
        "[10 20 30]",
        &[
            ("IndexOptions", "Index vector (dialog)"),
            ("Indices", "[3 1]"),
        ],
    )
    .unwrap();
    assert_eq!((s["o[1]"][0], s["o[2]"][0]), (30.0, 10.0));
}

#[test]
fn selector_spellings_must_agree_when_both_are_stored() {
    // BlockParameterDefaults merge into the block, so a programmatic spelling
    // beside the file's own must not win silently when they differ.
    let e = select(
        "[10 20 30]",
        &[
            ("IndexOptions", "Index vector (dialog)"),
            ("Indices", "[2 3]"),
            ("IndexParamArray", "{'[1 3]'}"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("Indices and IndexParamArray disagree"), "{e}");
    // Equal items in either quoting agree.
    let s = select(
        "[10 20 30]",
        &[
            ("IndexOptions", "Index vector (dialog)"),
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("Indices", "[2 3]"),
            ("IndexParamArray", "{'[2 3]'}"),
        ],
    )
    .unwrap();
    assert_eq!((s["o[1]"][0], s["o[2]"][0]), (20.0, 30.0));
    // Diagnostics name the spelling the block stores (0 is outside one-based).
    let e = select(
        "[1 2]",
        &[
            ("IndexOptions", "Index vector (dialog)"),
            ("Indices", "[0]"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("Indices[1]"), "{e}");
    let e = select(
        "[1 2]",
        &[
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("IndexParamArray", "{'[0]'}"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("IndexParamArray[1]"), "{e}");
}

#[test]
fn legacy_selector_elements_follow_the_same_rules() {
    let s = select(
        "[10 20 30]",
        &[("Elements", "[3 1]"), ("InputPortWidth", "3")],
    )
    .unwrap();
    assert_eq!((s["o[1]"][0], s["o[2]"][0]), (30.0, 10.0));
    let s = select(
        "[10 20 30]",
        &[("Elements", "[3 1]"), ("InputPortWidth", "-1")],
    )
    .unwrap();
    assert_eq!(s["o[1]"], vec![30.0]);
    let e = select(
        "[10 20 30]",
        &[("Elements", "[3 1]"), ("InputPortWidth", "4")],
    )
    .unwrap_err();
    assert!(e.contains("InputPortWidth 4 does not match"), "{e}");
    let e = select(
        "[10 20 30]",
        &[("Elements", "[3 1]"), ("ElementSrc", "External")],
    )
    .unwrap_err();
    assert!(e.contains("ElementSrc=External"), "{e}");
}

#[test]
fn two_dimensional_selector_takes_rows_and_columns() {
    let matrix = "[1 2 3;4 5 6]";
    let row = select(
        matrix,
        &[
            ("NumberOfDimensions", "2"),
            ("IndexOptionArray", "{'Index vector (dialog)','Select all'}"),
            ("IndexParamArray", "{'2','1'}"),
        ],
    )
    .unwrap();
    assert_eq!(
        (row["o[1,1]"][0], row["o[1,2]"][0], row["o[1,3]"][0]),
        (4.0, 5.0, 6.0)
    );
    let column = select(
        matrix,
        &[
            ("NumberOfDimensions", "2"),
            ("IndexOptionArray", "{'Select all','Index vector (dialog)'}"),
            ("IndexParamArray", "{'1','3'}"),
        ],
    )
    .unwrap();
    assert_eq!((column["o[1,1]"][0], column["o[2,1]"][0]), (3.0, 6.0));
    let both = select(
        matrix,
        &[
            ("NumberOfDimensions", "2"),
            (
                "IndexOptionArray",
                "{'Index vector (dialog)','Index vector (dialog)'}",
            ),
            ("IndexParamArray", "{'2','[3 1]'}"),
        ],
    )
    .unwrap();
    assert_eq!((both["o[1,1]"][0], both["o[1,2]"][0]), (6.0, 4.0));
    let element = select(
        matrix,
        &[
            ("NumberOfDimensions", "2"),
            ("IndexMode", "Zero-based"),
            (
                "IndexOptionArray",
                "{'Index vector (dialog)','Index vector (dialog)'}",
            ),
            ("IndexParamArray", "{'1','2'}"),
        ],
    )
    .unwrap();
    assert_eq!(element["o"], vec![6.0]);
}

#[test]
fn selector_rejects_what_it_does_not_implement() {
    let dialog = [
        ("IndexOptionArray", "{'Index vector (dialog)'}"),
        ("IndexParamArray", "{'[3 1]'}"),
    ];
    let e = select("[1 2]", &dialog).unwrap_err();
    assert!(e.contains("index 3 exceeds"), "{e}");
    let e = select(
        "[1 2 3]",
        &[("IndexOptionArray", "{'Index vector (port)'}")],
    )
    .unwrap_err();
    assert!(e.contains("port-driven"), "{e}");
    let e = select(
        "[1 2 3]",
        &[
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("NumberOfDimensions", "3"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("NumberOfDimensions=3"), "{e}");
    let e = select("[1 2;3 4]", &dialog).unwrap_err();
    assert!(e.contains("NumberOfDimensions=2"), "{e}");
    let e = select(
        "[1 2 3]",
        &[
            ("NumberOfDimensions", "2"),
            ("IndexOptionArray", "{'Select all','Select all'}"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("row/column orientation"), "{e}");
    let e = select(
        "[1 2 3]",
        &[
            ("IndexMode", "Two-based"),
            ("IndexOptionArray", "{'Select all'}"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("IndexMode=Two-based"), "{e}");
    let e = select(
        "[1 2 3]",
        &[
            ("IndexOptionArray", "{'Index vector (dialog)'}"),
            ("IndexParamArray", "{'0'}"),
        ],
    )
    .unwrap_err();
    assert!(e.contains("outside the 1-based range"), "{e}");
}

/// Clock -> control of a Multiport Switch over constants 10, 20, 30 (and 99
/// on an additional default port when `extra`), sampled every half second.
fn clocked_multiport(params: &[(&str, &str)], extra: bool) -> Result<Vec<f64>, String> {
    let mut blocks = vec![
        block("clock", "Clock", &[]),
        block("a", "Constant", &[("Value", "10")]),
        block("b", "Constant", &[("Value", "20")]),
        block("c", "Constant", &[("Value", "30")]),
        block("mps", "MultiPortSwitch", params),
    ];
    let mut lines = vec![
        line("clock", 1, "mps", 1),
        line("a", 1, "mps", 2),
        line("b", 1, "mps", 3),
        line("c", 1, "mps", 4),
    ];
    if extra {
        blocks.push(block("d", "Constant", &[("Value", "99")]));
        lines.push(line("d", 1, "mps", 5));
    }
    let options = Options {
        stop: 4.0,
        step: 0.5,
        solver: Solver::Euler,
        ..Options::default()
    };
    simulate_model(&model(blocks, lines), &options)
        .map(|t| t.signals["mps"].clone())
        .map_err(|e| e.to_string())
}

#[test]
fn multiport_switch_follows_a_clock_index_with_default_and_error_paths() {
    // One-based: t in [1,2) -> 10, [2,3) -> 20, [3,4] -> 30; t < 1 truncates
    // to 0, matches nothing and takes the last data port.
    let one_based = [
        ("Inputs", "3"),
        ("DataPortOrder", "One-based contiguous"),
        ("DiagnosticForDefault", "None"),
    ];
    assert_eq!(
        clocked_multiport(&one_based, false).unwrap(),
        vec![30.0, 30.0, 10.0, 10.0, 20.0, 20.0, 30.0, 30.0, 30.0]
    );
    // Zero-based: t in [0,1) -> 10 ... t = 3 and beyond is out of range.
    let zero_based = [
        ("Inputs", "3"),
        ("DataPortOrder", "Zero-based contiguous"),
        ("DiagnosticForDefault", "Warning"),
    ];
    assert_eq!(
        clocked_multiport(&zero_based, false).unwrap(),
        vec![10.0, 10.0, 20.0, 20.0, 30.0, 30.0, 30.0, 30.0, 30.0]
    );
    // Legacy zeroidx spelling of the port order.
    let legacy = [
        ("Inputs", "3"),
        ("zeroidx", "on"),
        ("DiagnosticForDefault", "None"),
    ];
    assert_eq!(
        clocked_multiport(&legacy, false).unwrap(),
        clocked_multiport(&zero_based, false).unwrap()
    );
    // An additional data port carries the default case.
    let additional = [
        ("Inputs", "3"),
        ("DataPortOrder", "One-based contiguous"),
        ("DataPortForDefault", "Additional data port"),
        ("DiagnosticForDefault", "None"),
    ];
    assert_eq!(
        clocked_multiport(&additional, true).unwrap(),
        vec![99.0, 99.0, 10.0, 10.0, 20.0, 20.0, 30.0, 30.0, 99.0]
    );
    // Explicit indices: control 0 or 1 -> 10, 3 -> 20, 2 -> 30.
    let explicit = [
        ("Inputs", "3"),
        ("DataPortOrder", "Specify indices"),
        ("DataPortIndices", "{[0 1],3,2}"),
        ("DiagnosticForDefault", "None"),
    ];
    assert_eq!(
        clocked_multiport(&explicit, false).unwrap(),
        vec![10.0, 10.0, 10.0, 10.0, 30.0, 30.0, 20.0, 20.0, 30.0]
    );
    // The documented default diagnostic stops the run on the first miss.
    let strict = [("Inputs", "3"), ("DataPortOrder", "One-based contiguous")];
    let e = clocked_multiport(&strict, false).unwrap_err();
    assert!(
        e.contains("selects no data port") && e.contains("time 0"),
        "{e}"
    );
    for (params, needle) in [
        (
            vec![
                ("Inputs", "3"),
                ("DataPortOrder", "Specify indices"),
                ("DataPortIndices", "{[0 1],1,2}"),
            ],
            "more than one data port",
        ),
        (
            vec![
                ("Inputs", "3"),
                ("DataPortOrder", "Specify indices"),
                ("DataPortIndices", "{0,1}"),
            ],
            "2 entries for 3 data ports",
        ),
        (
            vec![("Inputs", "3"), ("AllowDiffInputSizes", "on")],
            "AllowDiffInputSizes",
        ),
        (
            vec![("Inputs", "3"), ("DataPortOrder", "Enumerated")],
            "DataPortOrder=Enumerated",
        ),
    ] {
        let e = clocked_multiport(&params, false).unwrap_err();
        assert!(e.contains(needle), "{e}");
    }
}

#[test]
fn multiport_switch_broadcasts_vector_data_under_a_scalar_control() {
    let m = model(
        vec![
            block("k", "Constant", &[("Value", "2")]),
            block("a", "Constant", &[("Value", "[1 2]")]),
            block("b", "Constant", &[("Value", "7")]),
            block(
                "mps",
                "MultiPortSwitch",
                &[("Inputs", "2"), ("DataPortOrder", "One-based contiguous")],
            ),
        ],
        vec![
            line("k", 1, "mps", 1),
            line("a", 1, "mps", 2),
            line("b", 1, "mps", 3),
        ],
    );
    let t = simulate_model(&m, &once()).unwrap();
    assert_eq!((t.signals["mps[1]"][0], t.signals["mps[2]"][0]), (7.0, 7.0));
    let mut vector_control = m.clone();
    vector_control.root.blocks[0]
        .parameters
        .insert("Value".into(), "[1 2]".into());
    assert!(error(&vector_control).contains("control input must be scalar"));
}

/// Index Vector: control `k` indexes `[18 15 17 10]` zero-based.
fn index_vector(control: &str, subject: Block) -> Result<f64, String> {
    let m = model(
        vec![
            block("k", "Constant", &[("Value", control)]),
            block("v", "Constant", &[("Value", "[18 15 17 10]")]),
            subject,
        ],
        vec![line("k", 1, "iv", 1), line("v", 1, "iv", 2)],
    );
    simulate_model(&m, &once())
        .map(|t| t.signals["iv"][0])
        .map_err(|e| e.to_string())
}

#[test]
fn index_vector_selects_an_element_zero_based() {
    let native = || {
        block(
            "iv",
            "MultiPortSwitch",
            &[("Inputs", "1"), ("DataPortOrder", "Zero-based contiguous")],
        )
    };
    assert_eq!(index_vector("3", native()).unwrap(), 10.0);
    assert_eq!(index_vector("0", native()).unwrap(), 18.0);
    // Controls truncate toward zero before indexing.
    assert_eq!(index_vector("2.9", native()).unwrap(), 17.0);
    let e = index_vector("4", native()).unwrap_err();
    assert!(e.contains("selects no data port"), "{e}");
    let mut lenient = native();
    lenient
        .parameters
        .insert("DiagnosticForDefault".into(), "None".into());
    assert_eq!(index_vector("4", lenient).unwrap(), 10.0);
    // The library block, as a linked Reference storing no parameters of its
    // own, with the wrapped library path files use.
    let linked = library("iv", "simulink/Signal\nRouting/Index\nVector", 2, &[]);
    assert_eq!(index_vector("3", linked.clone()).unwrap(), 10.0);
    assert_eq!(index_vector("1", linked).unwrap(), 15.0);
    let mut one_based = native();
    one_based
        .parameters
        .insert("DataPortOrder".into(), "One-based contiguous".into());
    assert_eq!(index_vector("1", one_based).unwrap(), 18.0);
    let mut explicit = native();
    explicit
        .parameters
        .insert("DataPortOrder".into(), "Specify indices".into());
    let e = index_vector("1", explicit).unwrap_err();
    assert!(e.contains("contiguous DataPortOrder"), "{e}");
}

#[test]
fn manual_switch_passes_either_input_by_position() {
    let switch = |sw: &[(&str, &str)]| {
        model(
            vec![
                block("a", "Constant", &[("Value", "5")]),
                block("b", "Constant", &[("Value", "[7 8]")]),
                block("sw", "ManualSwitch", sw),
            ],
            vec![line("a", 1, "sw", 1), line("b", 1, "sw", 2)],
        )
    };
    // sw=1 connects the first input (expanded against the vector), sw=0 the second.
    let t = simulate_model(&switch(&[("sw", "1")]), &once()).unwrap();
    assert_eq!((t.signals["sw[1]"][0], t.signals["sw[2]"][0]), (5.0, 5.0));
    let t = simulate_model(&switch(&[("sw", "0")]), &once()).unwrap();
    assert_eq!((t.signals["sw[1]"][0], t.signals["sw[2]"][0]), (7.0, 8.0));
    // Files store the position as CurrentSetting (the corpus instance, mavlink
    // test_messages_local.slx SID 55, stores 0 with no sw anywhere); the
    // programmatic sw wins when both are present.
    let t = simulate_model(&switch(&[("CurrentSetting", "0")]), &once()).unwrap();
    assert_eq!((t.signals["sw[1]"][0], t.signals["sw[2]"][0]), (7.0, 8.0));
    let t = simulate_model(&switch(&[("CurrentSetting", "1")]), &once()).unwrap();
    assert_eq!((t.signals["sw[1]"][0], t.signals["sw[2]"][0]), (5.0, 5.0));
    let both = switch(&[("sw", "1"), ("CurrentSetting", "0")]);
    let t = simulate_model(&both, &once()).unwrap();
    assert_eq!((t.signals["sw[1]"][0], t.signals["sw[2]"][0]), (5.0, 5.0));
    assert!(error(&switch(&[])).contains("sw or CurrentSetting"));
    assert!(error(&switch(&[("sw", "2")])).contains("sw=2"));
    assert!(error(&switch(&[("CurrentSetting", "2")])).contains("CurrentSetting=2"));
    assert!(error(&switch(&[("sw", "1"), ("varsize", "on")])).contains("varsize"));
}

#[test]
fn environment_controller_passes_the_sim_input() {
    let controller = |sim: &str, coder: &str| {
        model(
            vec![
                block("sim", "Constant", &[("Value", sim)]),
                block("coder", "Constant", &[("Value", coder)]),
                library(
                    "env",
                    "simulink/Signal Routing/Environment Controller",
                    2,
                    &[],
                ),
            ],
            vec![line("sim", 1, "env", 1), line("coder", 1, "env", 2)],
        )
    };
    let t = simulate_model(&controller("[5 6]", "[9 9]"), &once()).unwrap();
    assert_eq!((t.signals["env[1]"][0], t.signals["env[2]"][0]), (5.0, 6.0));
    assert!(error(&controller("5", "[9 9]")).contains("identical dimensions"));
    // Static diagnostics no longer flag these links as unsupported, while
    // other library links still are.
    let report = diagnose(&controller("5", "9"), &DiagnosticContext::default());
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "simulation_link_or_mask"));
    let mut other = controller("5", "9");
    other.root.blocks[2].library_source = Some("simulink/Signal Routing/Merge".into());
    let report = diagnose(&other, &DiagnosticContext::default());
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "simulation_link_or_mask"));
    assert!(error(&other).contains("library links"));
}

#[test]
fn switch_threshold_criteria_compare_at_the_boundary() {
    let switch = |control: &str, params: &[(&str, &str)]| {
        let m = model(
            vec![
                block("a", "Constant", &[("Value", "1")]),
                block("u2", "Constant", &[("Value", control)]),
                block("b", "Constant", &[("Value", "0")]),
                block("sw", "Switch", params),
            ],
            vec![
                line("a", 1, "sw", 1),
                line("u2", 1, "sw", 2),
                line("b", 1, "sw", 3),
            ],
        );
        simulate_model(&m, &once())
            .map(|t| t.signals["sw"][0])
            .map_err(|e| e.to_string())
    };
    let at_least = [("Criteria", "u2 >= Threshold"), ("Threshold", "0.5")];
    let above = [("Criteria", "u2 > Threshold"), ("Threshold", "0.5")];
    assert_eq!(switch("0.5", &at_least).unwrap(), 1.0);
    assert_eq!(switch("0.5", &above).unwrap(), 0.0);
    assert_eq!(switch("0.4", &at_least).unwrap(), 0.0);
    assert_eq!(switch("0.6", &above).unwrap(), 1.0);
    // Imported blocks without Criteria use the documented `u2 >= Threshold`
    // with Threshold 0.
    assert_eq!(switch("0", &[]).unwrap(), 1.0);
    assert_eq!(switch("-1e-300", &[]).unwrap(), 0.0);
    // The nonzero criterion ignores (and never evaluates) the threshold.
    assert_eq!(
        switch(
            "0.1",
            &[("Criteria", "u2 ~= 0"), ("Threshold", "undefined_name")]
        )
        .unwrap(),
        1.0
    );
    let e = switch(
        "0.1",
        &[("Criteria", "u2 >= Threshold"), ("Threshold", "[1 2]")],
    )
    .unwrap_err();
    assert!(e.contains("array-valued Threshold"), "{e}");
}
