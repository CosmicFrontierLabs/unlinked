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
    }
}
fn model() -> Model {
    Model {
        name: "test".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
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
    m.root.blocks[1].block_type = "TransferFcn".into();
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
