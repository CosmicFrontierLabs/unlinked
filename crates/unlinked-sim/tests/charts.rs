use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::{compile, simulate_model, Options};

fn block(id: &str, kind: &str, name: &str, inputs: u32, outputs: u32) -> Block {
    Block {
        id: id.into(),
        name: name.into(),
        block_type: kind.into(),
        position: Rect::default(),
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts {
            inputs,
            outputs,
            ..PortCounts::default()
        },
        parameters: BTreeMap::new(),
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
fn data(id: &str, name: &str, scope: DataScope) -> ChartData {
    ChartData {
        id: id.into(),
        name: name.into(),
        scope,
        port: Some(1),
        size: Some("-1".into()),
        variable_size: Some("0".into()),
        complexity: Some("SF_COMPLEX_INHERITED".into()),
        data_type: Some("Inherit: Same as Simulink".into()),
    }
}
fn model() -> Model {
    let source = block("source", "Clock", "time", 0, 1);
    let mut function = block("f", "SubSystem", "f/escaped", 1, 1);
    function
        .parameters
        .insert("SFBlockType".into(), "MATLAB Function".into());
    function
        .parameters
        .insert("TreatAsAtomicUnit".into(), "on".into());
    let mut engine = block("sf", "S-Function", " SFunction ", 1, 2);
    engine
        .parameters
        .insert("FunctionName".into(), "sf_sfun".into());
    function.subsystem = Some(Box::new(System {
        blocks: vec![
            block("in", "Inport", "u", 0, 1),
            engine,
            block("out", "Outport", "y", 1, 0),
        ],
        lines: vec![line("in", 1, "sf", 1), line("sf", 2, "out", 1)],
        ..System::default()
    }));
    let script = "function y=f(u)\ny=2*u+1;\nend".to_owned();
    Model {
        name: "chart-test".into(),
        source: SourceFormat::Slx,
        simulink_version: None,
        config: SimConfig::default(),
        workspace: BTreeMap::new(),
        root: System {
            blocks: vec![source, function],
            lines: vec![line("source", 1, "f", 1)],
            ..System::default()
        },
        charts: vec![Chart {
            id: "chart".into(),
            name: "f//escaped".into(),
            kind: ChartKind::MatlabFunction,
            states: vec![State {
                id: "function".into(),
                label: "f".into(),
                position: Rect::default(),
                parent: None,
                subviewer: Some("chart".into()),
                kind: StateKind::Function,
                script: Some(script.clone()),
            }],
            transitions: vec![],
            junctions: vec![],
            data: vec![
                data("in", "u", DataScope::Input),
                data("out", "y", DataScope::Output),
            ],
            script: Some(script),
            update_method: Some("INHERITED".into()),
            sample_time: Some("-1".into()),
        }],
    }
}
fn options() -> Options {
    Options {
        stop: 0.3,
        step: 0.1,
        ..Options::default()
    }
}
#[test]
fn pure_scalar_chart_maps_escaped_path_ports_and_function() {
    let m = model();
    let trace = simulate_model(&m, &options()).unwrap();
    for (i, t) in trace.time.iter().enumerate() {
        assert!((trace.signals["f"][i] - (2. * t + 1.)).abs() < 1e-12);
    }
    assert_eq!(
        m.root.blocks[1].block_type, "SubSystem",
        "lowering must not mutate imported model"
    );
}
#[test]
fn chart_paths_are_exact_and_ambiguous_charts_fail() {
    let mut m = model();
    m.charts[0].name = "f/escaped".into();
    assert!(compile(&m, &options()).is_err());
    let mut m = model();
    m.charts.push(m.charts[0].clone());
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("duplicate chart"));
    let mut m = model();
    m.root.blocks.push(m.root.blocks[1].clone());
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
}
#[test]
fn unsupported_data_timing_and_state_semantics_fail_closed() {
    for edit in 0..14 {
        let mut m = model();
        let chart = &mut m.charts[0];
        match edit {
            0 => chart.data[0].size = Some("[2 1]".into()),
            1 => chart.data[0].variable_size = Some("1".into()),
            2 => chart.data[0].complexity = Some("SF_COMPLEX".into()),
            3 => chart.data[0].data_type = Some("uint8".into()),
            4 => chart.data[0].scope = DataScope::Parameter,
            5 => chart.data[0].port = Some(2),
            6 => chart.data[0].name = "different".into(),
            7 => chart.update_method = Some("DISCRETE".into()),
            8 => chart.sample_time = Some("0.1".into()),
            9 => chart.kind = ChartKind::StateChart,
            10 => chart.states[0].kind = StateKind::Or,
            11 => chart.states[0].parent = Some("nested".into()),
            12 => chart.data.push(chart.data[0].clone()),
            13 => chart.script = None,
            _ => unreachable!(),
        }
        assert!(compile(&m, &options()).is_err(), "edit {edit}");
    }
}
#[test]
fn backing_subsystem_content_and_connections_cannot_disappear() {
    for edit in 0..10 {
        let mut m = model();
        let backing = &mut m.root.blocks[1];
        match edit {
            0 => backing.ports.enable = 1,
            1 => backing.library_source = Some("unknown/library".into()),
            2 => {
                backing
                    .parameters
                    .insert("SFBlockType".into(), "Chart".into());
            }
            3 => backing.subsystem = None,
            4 => backing
                .subsystem
                .as_mut()
                .unwrap()
                .blocks
                .push(block("extra", "Gain", "extra", 1, 1)),
            5 => backing.subsystem.as_mut().unwrap().lines.clear(),
            6 => {
                backing.subsystem.as_mut().unwrap().lines[1]
                    .src
                    .as_mut()
                    .unwrap()
                    .port
                    .index = 1
            }
            7 => backing.subsystem.as_mut().unwrap().blocks[0].name = "other".into(),
            8 => {
                backing.subsystem.as_mut().unwrap().blocks[0]
                    .parameters
                    .insert("OutDataTypeStr".into(), "uint8".into());
            }
            9 => {
                backing.parameters.insert("Commented".into(), "on".into());
            }
            _ => unreachable!(),
        }
        assert!(compile(&m, &options()).is_err(), "edit {edit}");
    }
}

#[test]
fn imported_corpus_scalar_function_runs_unchanged_in_isolation() {
    let Some(root) = std::env::var_os("UNLINKED_TEST_CASES") else {
        eprintln!("UNLINKED_TEST_CASES unset; isolated imported chart test skipped");
        return;
    };
    let file = std::path::PathBuf::from(root).join("fixtures/uuv/models/rovSim_los.slx");
    let mut imported =
        unlinked_import::import("rovSim_los.slx", &std::fs::read(file).unwrap()).unwrap();
    let pid = imported
        .root
        .blocks
        .iter()
        .find(|b| b.name == "PID Control")
        .unwrap();
    let function = pid
        .subsystem
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find(|b| b.name == "ud")
        .unwrap()
        .clone();
    let chart = imported
        .charts
        .iter()
        .find(|c| c.name == "PID Control/ud")
        .unwrap()
        .clone();
    // Run this existing function with its original script, declarations and
    // backing wrapper; the rest of the ROV model requires unsupported blocks.
    // Expected values follow the source formula, not an independent Simulink run.
    let function_id = function.id.0.clone();
    let mut u = block("test-u", "Constant", "test-u", 0, 1);
    u.parameters.insert("Value".into(), "4".into());
    let mut angle = block("test-angle", "Constant", "test-angle", 0, 1);
    angle.parameters.insert("Value".into(), "pi/4".into());
    imported.root = System {
        blocks: vec![u, angle, function],
        lines: vec![
            line("test-u", 1, &function_id, 1),
            line("test-angle", 1, &function_id, 2),
        ],
        ..System::default()
    };
    imported.workspace.clear();
    imported.charts = vec![Chart {
        name: "ud".into(),
        ..chart
    }];
    let trace = simulate_model(&imported, &options()).unwrap();
    assert!(trace.signals[&function_id]
        .iter()
        .all(|x| (*x - 2.).abs() < 1e-12));
    imported.root.blocks[1]
        .parameters
        .insert("Value".into(), "pi".into());
    let trace = simulate_model(&imported, &options()).unwrap();
    assert!(trace.signals[&function_id].iter().all(|x| *x == 0.));
}

#[test]
fn only_canonical_legacy_kernel_control_flow_is_accepted() {
    let mut m = model();
    let chart = &mut m.charts[0];
    chart.states[0].label = "eML_blk_kernel()".into();
    chart.transitions = vec![Transition {
        id: "tr".into(),
        label: "{eML_blk_kernel();}".into(),
        src: None,
        dst: Some("junction".into()),
        points: vec![],
        label_position: None,
        subviewer: Some(chart.id.clone()),
    }];
    chart.junctions = vec![Junction {
        id: "junction".into(),
        position: Rect::default(),
        kind: JunctionKind::Connective,
        subviewer: Some(chart.id.clone()),
    }];
    assert!(compile(&m, &options()).is_ok());
    for edit in 0..5 {
        let mut m = m.clone();
        let chart = &mut m.charts[0];
        match edit {
            0 => chart.transitions[0].label = "{eML_blk_kernel(); y=5;}".into(),
            1 => chart.transitions[0].src = Some("junction".into()),
            2 => chart.junctions[0].kind = JunctionKind::History,
            3 => chart.transitions[0].subviewer = Some("nested".into()),
            4 => chart.transitions[0].dst = Some("other".into()),
            _ => unreachable!(),
        }
        assert!(compile(&m, &options()).is_err(), "edit {edit}");
    }
}

#[test]
fn wrapper_sample_time_must_be_inherited_and_real_complexity_is_recognized() {
    let mut m = model();
    for data in &mut m.charts[0].data {
        data.complexity = Some("SF_COMPLEX_NO".into());
    }
    m.root.blocks[1]
        .parameters
        .insert("SystemSampleTime".into(), " -1 ".into());
    assert!(compile(&m, &options()).is_ok());
    for sample in ["0.1", "0", "[-1 0]", "garbage"] {
        m.root.blocks[1]
            .parameters
            .insert("SystemSampleTime".into(), sample.into());
        assert!(compile(&m, &options()).is_err(), "{sample}");
    }
}
