use std::collections::BTreeMap;
use unlinked_model::*;
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
fn model() -> Model {
    Model {
        name: "test".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        type_defaults: Default::default(),
        charts: Vec::new(),
        workspace: BTreeMap::from([
            ("gain".into(), "twice/2".into()),
            ("twice".into(), "4".into()),
        ]),
        root: System {
            blocks: vec![
                block("c", "Constant", &[("Value", "3")]),
                block("g", "Gain", &[("Gain", "gain")]),
            ],
            lines: vec![Line {
                src: Some(Endpoint {
                    block: "c".into(),
                    port: PortRef {
                        kind: PortKind::Out,
                        index: 1,
                    },
                }),
                dst: Some(Endpoint {
                    block: "g".into(),
                    port: PortRef {
                        kind: PortKind::In,
                        index: 1,
                    },
                }),
                ..Line::default()
            }],
            ..System::default()
        },
    }
}
#[test]
fn evaluates_workspace_dependencies_and_imported_parameters() {
    let t = simulate_model(
        &model(),
        &Options {
            stop: 0.0,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(t.signals["g"], vec![6.0]);
}
#[test]
fn rejects_unsupported_semantics_and_unknown_parameters() {
    let mut m = model();
    m.root.blocks[1].block_type = "UnsupportedTransfer".into();
    assert!(compile(&m, &Options::default())
        .unwrap_err()
        .to_string()
        .contains("unsupported block type"));
    let mut m = model();
    m.root.blocks[1]
        .parameters
        .insert("Gain".into(), "unknown".into());
    assert!(compile(&m, &Options::default()).is_err());
    let mut m = model();
    m.root.blocks[1]
        .parameters
        .insert("OutDataTypeStr".into(), "int8".into());
    assert!(compile(&m, &Options::default()).is_err());
    let mut m = model();
    m.workspace.insert("pi".into(), "twice".into());
    assert!(compile(&m, &Options::default()).is_err());
}
#[test]
fn rejects_multirate_and_off_grid_steps() {
    let mut m = model();
    m.root.blocks[1]
        .parameters
        .insert("SampleTime".into(), "0.2".into());
    assert!(compile(&m, &Options::default()).is_err());
    let mut m = model();
    m.root.blocks[0] = block("c", "Step", &[("Time", "0.305")]);
    assert!(compile(&m, &Options::default()).is_err());
}

#[test]
fn virtual_subsystem_interfaces_are_lowered() {
    let mut m = model();
    let mut sub = block("sub", "SubSystem", &[]);
    let ep = |id: &str, kind, index| Endpoint {
        block: id.into(),
        port: PortRef { kind, index },
    };
    let line = |a: &str, b: &str| Line {
        src: Some(ep(a, PortKind::Out, 1)),
        dst: Some(ep(b, PortKind::In, 1)),
        ..Line::default()
    };
    sub.subsystem = Some(Box::new(System {
        blocks: vec![
            block("in", "Inport", &[]),
            block("gain", "Gain", &[("Gain", "4")]),
            block("out", "Outport", &[]),
        ],
        lines: vec![line("in", "gain"), line("gain", "out")],
        ..System::default()
    }));
    m.root.blocks.insert(1, sub);
    m.root.lines = vec![line("c", "sub"), line("sub", "g")];
    let t = simulate_model(
        &m,
        &Options {
            stop: 0.0,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(t.signals["g"], vec![24.0]);
    assert_eq!(t.signals["sub/out"], vec![12.0]);
    m.root.blocks[1]
        .parameters
        .insert("TreatAsAtomicUnit".into(), "on".into());
    assert!(compile(&m, &Options::default()).is_err());
}

#[test]
fn logical_import_accepts_boolean_outputs_and_rejects_unsupported_modes() {
    let mut m = model();
    m.root.blocks[1] = block(
        "g",
        "Logic",
        &[("Operator", "NOT"), ("OutDataTypeStr", "boolean")],
    );
    let trace = simulate_model(
        &m,
        &Options {
            stop: 0.0,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(trace.signals["g"], vec![0.0]);
    m.root.blocks[1] = block(
        "g",
        "Logic",
        &[("Operator", "OR"), ("Inputs", "4294967295")],
    );
    assert!(compile(&m, &Options::default()).is_err());
    m.root.blocks[1] = block("g", "Switch", &[("Criteria", "u2 > Threshold")]);
    assert!(compile(&m, &Options::default())
        .unwrap_err()
        .to_string()
        .contains("datatype propagation"));
    m.root.blocks[1] = block("g", "RelationalOperator", &[("ZeroCross", "on")]);
    assert!(compile(&m, &Options::default())
        .unwrap_err()
        .to_string()
        .contains("ZeroCross"));
}

#[test]
fn commented_blocks_and_subsystems_never_execute_silently() {
    for kind in ["Gain", "Goto", "SubSystem"] {
        for setting in ["on", "through"] {
            let mut m = model();
            m.root.blocks[1].block_type = kind.into();
            m.root.blocks[1]
                .parameters
                .insert("Commented".into(), setting.into());
            assert!(compile(&m, &Options::default())
                .unwrap_err()
                .to_string()
                .contains("commented"));
        }
    }
    let mut m = model();
    let mut sub = block("sub", "SubSystem", &[]);
    sub.subsystem = Some(Box::new(System {
        blocks: vec![block("nested", "Gain", &[("Commented", "on")])],
        ..System::default()
    }));
    m.root.blocks.push(sub);
    assert!(compile(&m, &Options::default())
        .unwrap_err()
        .to_string()
        .contains("commented"));
}
