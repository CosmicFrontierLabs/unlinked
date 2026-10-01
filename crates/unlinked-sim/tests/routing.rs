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
        name: "vectors".into(),
        source: SourceFormat::Mdl,
        charts: vec![],
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
fn options() -> Options {
    Options {
        stop: 0.0,
        ..Options::default()
    }
}

#[test]
fn local_tags_route_vectors_and_detect_ambiguity() {
    let mut m = model(
        vec![
            block("c", "Constant", &[("Value", "[2;3]")]),
            block("g", "Goto", &[("GotoTag", "A")]),
            block("f", "From", &[("GotoTag", "A")]),
            block("o", "Outport", &[]),
        ],
        vec![line("c", 1, "g", 1), line("f", 1, "o", 1)],
    );
    let trace = simulate_model(&m, &options()).unwrap();
    assert_eq!(trace.signals["o[1]"], vec![2.0]);
    assert_eq!(trace.signals["o[2]"], vec![3.0]);
    m.root
        .blocks
        .push(block("duplicate", "Goto", &[("GotoTag", "A")]));
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
}
#[test]
fn unsupported_visibility_and_missing_tags_fail() {
    for visibility in ["global", "scoped"] {
        let m = model(
            vec![block(
                "g",
                "Goto",
                &[("GotoTag", "A"), ("TagVisibility", visibility)],
            )],
            vec![],
        );
        assert!(compile(&m, &options())
            .unwrap_err()
            .to_string()
            .contains("only local"));
    }
    let m = model(vec![block("f", "From", &[("GotoTag", "missing")])], vec![]);
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("no matching"));
}

#[test]
fn local_tags_do_not_cross_subsystem_boundaries() {
    let mut child = block("sub", "SubSystem", &[]);
    child.subsystem = Some(Box::new(System {
        blocks: vec![
            block("c", "Constant", &[("Value", "1")]),
            block("g", "Goto", &[("GotoTag", "A")]),
        ],
        lines: vec![line("c", 1, "g", 1)],
        ..System::default()
    }));
    let m = model(vec![child, block("f", "From", &[("GotoTag", "A")])], vec![]);
    assert!(compile(&m, &options())
        .unwrap_err()
        .to_string()
        .contains("no matching Goto in this subsystem"));
}
