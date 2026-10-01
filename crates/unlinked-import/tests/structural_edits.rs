//! Structural edit cases found in review: each would let a batch write a
//! file that differs from the edited IR, or let an edit through that adds
//! a structural error.

use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, next_sid, structural_regression, DisconnectPolicy, Edit};
use unlinked_model::{Endpoint, Line, Model, PortKind, PortRef, Rect};

fn lconn(block: &str) -> Endpoint {
    Endpoint {
        block: block.into(),
        port: PortRef {
            kind: PortKind::LConn,
            index: 1,
        },
    }
}

/// Disconnecting one end of a physical connection, whose branches store
/// their ports as `Src`, persists exactly as in the IR.
fn physical_disconnect_roundtrips(name: &str, bytes: &[u8]) {
    let edits = [Edit::Disconnect {
        system: vec![],
        dst: lconn("2"),
    }];
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    apply_batch(&mut expected, &edits).unwrap();
    assert_eq!(expected.root.lines[0].branches.len(), 1);
    let out = unlinked_import::patch::apply_edits(name, bytes, &edits).unwrap();
    let actual = unlinked_import::import(name, &out).unwrap();
    assert_eq!(actual.root.lines, expected.root.lines);
}

#[test]
fn physical_disconnect_roundtrips_mdl() {
    let src = "Model {\n Name \"m\"\n System {\n Block {\n BlockType Reference\n Name \"a\"\n SID \"1\"\n Ports [0, 0, 0, 0, 0, 1]\n }\n Block {\n BlockType Reference\n Name \"b\"\n SID \"2\"\n Ports [0, 0, 0, 0, 0, 1]\n }\n Line {\n Branch {\n Src \"1#lconn:1\"\n }\n Branch {\n Src \"2#lconn:1\"\n }\n }\n }\n}\n";
    physical_disconnect_roundtrips("m.mdl", src.as_bytes());
}

#[test]
fn physical_disconnect_roundtrips_slx() {
    let src = r#"<ModelInformation><Model Name="m"><System><Block BlockType="Reference" Name="a" SID="1"><P Name="Ports">[0,0,0,0,0,1]</P></Block><Block BlockType="Reference" Name="b" SID="2"><P Name="Ports">[0,0,0,0,0,1]</P></Block><Line><Branch><P Name="Src">1#lconn:1</P></Branch><Branch><P Name="Src">2#lconn:1</P></Branch></Line></System></Model></ModelInformation>"#;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(src.as_bytes()).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    physical_disconnect_roundtrips("m.slx", &bytes);
}

#[test]
fn allocated_sids_are_not_reused_without_an_existing_watermark() {
    let bytes = b"Model {\n Name \"m\"\n System {\n }\n}\n";
    let add = Edit::AddBlock {
        system: vec![],
        id: "1".into(),
        block_type: "Gain".into(),
        name: "a".into(),
        position: Rect::new(0.0, 0.0, 30.0, 30.0),
    };
    let added = unlinked_import::patch::apply_edits("m.mdl", bytes, &[add]).unwrap();
    let delete = Edit::DeleteBlock {
        system: vec![],
        id: "1".into(),
        disconnect: DisconnectPolicy::Disconnect,
    };
    let removed = unlinked_import::patch::apply_edits("m.mdl", &added, &[delete]).unwrap();
    let reimported = unlinked_import::import("m.mdl", &removed).unwrap();
    assert_eq!(next_sid(&reimported), Some(2));
}

fn two_sums() -> Model {
    unlinked_import::import(
        "m.mdl",
        b"Model {\n Name m\n System {\n Block {\n BlockType Sum\n Name a\n SID 1\n Inputs ++\n }\n Block {\n BlockType Sum\n Name b\n SID 2\n Inputs ++\n }\n }\n}\n",
    )
    .unwrap()
}

#[test]
fn an_existing_error_cannot_be_traded_for_a_new_one() {
    let mut before = two_sums();
    before.root.blocks[0]
        .parameters
        .insert("Inputs".into(), "0".into());
    let mut after = before.clone();
    after.root.blocks[0]
        .parameters
        .insert("Inputs".into(), "++".into());
    after.root.blocks[1]
        .parameters
        .insert("Inputs".into(), "0".into());
    assert!(structural_regression(&before, &after).is_err());
}

#[test]
fn truncated_validation_does_not_authorize_regressions() {
    let mut before = two_sums();
    before.root.lines = vec![Line {
        points: vec![unlinked_model::Point { x: 0.0, y: 0.0 }; 500_001],
        ..Default::default()
    }];
    assert!(unlinked_model::validation::validate_structure(&before).truncated);
    let mut after = before.clone();
    after.root.blocks[0]
        .parameters
        .insert("Inputs".into(), "0".into());
    assert!(structural_regression(&before, &after).is_err());
}

#[test]
fn unresolved_port_counts_do_not_allow_rewiring_stale_ports() {
    let mut m = two_sums();
    m.root.blocks[1].block_type = "Mux".into();
    m.root.blocks[1]
        .parameters
        .insert("Inputs".into(), "n".into());
    m.root.blocks[1].ports.inputs = 3;
    let connect = Edit::Connect {
        system: vec![],
        src: Endpoint {
            block: "1".into(),
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        },
        dst: Endpoint {
            block: "2".into(),
            port: PortRef {
                kind: PortKind::In,
                index: 3,
            },
        },
    };
    assert!(apply_batch(&mut m, &[connect]).is_err());
}

#[test]
fn warning_display_cap_allows_safe_edits_but_not_new_errors() {
    let mut before = two_sums();
    before.root.lines = vec![Line::default(); 1500];
    let report = unlinked_model::validation::validate_structure(&before);
    assert!(report.warnings_omitted && !report.truncated);
    let mut after = before.clone();
    after.root.blocks[0].name = "renamed".into();
    assert!(structural_regression(&before, &after).is_ok());
    after.root.blocks[0]
        .parameters
        .insert("Inputs".into(), "0".into());
    assert!(structural_regression(&before, &after).is_err());
}
