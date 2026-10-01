//! Port blocks inside a subsystem are the subsystem block's ports: editing
//! them must update the block and the connections outside it identically in
//! the IR and in the patched MDL and SLX files.

use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, next_sid, DisconnectPolicy, Edit};
use unlinked_model::{Endpoint, Model, PortKind, PortRef, Rect};

/// Root: constants a and b feed subsystem s's inputs 1 and 2; s's output
/// feeds terminator t. Inside s: In1 and In2 summed into Out1.
const MDL: &str = r#"Model {
  Name "m"
  System {
    Name "m"
    Block {
      BlockType Constant
      Name "a"
      SID "1"
      Position [10, 10, 40, 40]
    }
    Block {
      BlockType Constant
      Name "b"
      SID "2"
      Position [10, 60, 40, 90]
    }
    Block {
      BlockType SubSystem
      Name "s"
      SID "3"
      Ports [2, 1]
      Position [100, 20, 160, 80]
      System {
        Name "s"
        Block {
          BlockType Inport
          Name "In1"
          SID "4"
          Position [10, 10, 40, 25]
        }
        Block {
          BlockType Inport
          Name "In2"
          SID "5"
          Position [10, 60, 40, 75]
          Port "2"
        }
        Block {
          BlockType Sum
          Name "add"
          SID "6"
          Ports [2, 1]
          Position [100, 20, 120, 60]
          Inputs "++"
        }
        Block {
          BlockType Outport
          Name "Out1"
          SID "7"
          Position [200, 30, 230, 45]
        }
        Line {
          SrcBlock "In1"
          SrcPort 1
          DstBlock "add"
          DstPort 1
        }
        Line {
          SrcBlock "In2"
          SrcPort 1
          DstBlock "add"
          DstPort 2
        }
        Line {
          SrcBlock "add"
          SrcPort 1
          DstBlock "Out1"
          DstPort 1
        }
      }
    }
    Block {
      BlockType Terminator
      Name "t"
      SID "8"
      Position [220, 40, 240, 60]
    }
    Line {
      SrcBlock "a"
      SrcPort 1
      DstBlock "s"
      DstPort 1
    }
    Line {
      SrcBlock "b"
      SrcPort 1
      DstBlock "s"
      DstPort 2
    }
    Line {
      SrcBlock "s"
      SrcPort 1
      DstBlock "t"
      DstPort 1
    }
  }
}
"#;

/// The same model as an SLX package.
fn slx() -> Vec<u8> {
    let block = |ty: &str, name: &str, sid: &str, pos: &str, extra: &str| {
        format!(
            r#"<Block BlockType="{ty}" Name="{name}" SID="{sid}"><P Name="Position">{pos}</P>{extra}</Block>"#
        )
    };
    let line = |src: &str, dst: &str| {
        format!(r#"<Line><P Name="Src">{src}</P><P Name="Dst">{dst}</P></Line>"#)
    };
    let inner = [
        block("Inport", "In1", "4", "[10, 10, 40, 25]", ""),
        block(
            "Inport",
            "In2",
            "5",
            "[10, 60, 40, 75]",
            r#"<P Name="Port">2</P>"#,
        ),
        block(
            "Sum",
            "add",
            "6",
            "[100, 20, 120, 60]",
            r#"<P Name="Ports">[2, 1]</P><P Name="Inputs">++</P>"#,
        ),
        block("Outport", "Out1", "7", "[200, 30, 230, 45]", ""),
        line("4#out:1", "6#in:1"),
        line("5#out:1", "6#in:2"),
        line("6#out:1", "7#in:1"),
    ]
    .concat();
    let root = [
        block("Constant", "a", "1", "[10, 10, 40, 40]", ""),
        block("Constant", "b", "2", "[10, 60, 40, 90]", ""),
        block(
            "SubSystem",
            "s",
            "3",
            "[100, 20, 160, 80]",
            &format!(r#"<P Name="Ports">[2, 1]</P><System>{inner}</System>"#),
        ),
        block("Terminator", "t", "8", "[220, 40, 240, 60]", ""),
        line("1#out:1", "3#in:1"),
        line("2#out:1", "3#in:2"),
        line("3#out:1", "8#in:1"),
    ]
    .concat();
    let xml = format!(
        r#"<ModelInformation><Model Name="m"><System>{root}</System></Model></ModelInformation>"#
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

fn ep(block: &str, kind: PortKind, index: u32) -> Endpoint {
    Endpoint {
        block: block.into(),
        port: PortRef { kind, index },
    }
}

/// Apply `edits` to the file and to its IR; the patched file must re-import
/// to exactly the edited IR. Returns that IR.
fn roundtrip(name: &str, bytes: &[u8], edits: &[Edit]) -> Model {
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    apply_batch(&mut expected, edits).unwrap();
    let patched = unlinked_import::patch::apply_edits(name, bytes, edits).unwrap();
    let actual = unlinked_import::import(name, &patched).unwrap();
    assert_eq!(actual.root, expected.root, "{name}");
    actual
}

fn files() -> [(&'static str, Vec<u8>); 2] {
    [("m.mdl", MDL.as_bytes().to_vec()), ("m.slx", slx())]
}

fn inner_port(model: &Model, name: &str) -> Option<String> {
    let s = model.root.blocks[2].subsystem.as_ref().unwrap();
    s.blocks
        .iter()
        .find(|b| b.name == name)
        .map(|b| b.param("Port").unwrap_or("1").to_string())
}

#[test]
fn renumbering_a_port_block_moves_the_outer_connection() {
    for (name, bytes) in files() {
        let edit = Edit::SetParameter {
            system: vec!["3".into()],
            id: "5".into(),
            name: "Port".into(),
            value: "1".into(),
        };
        let m = roundtrip(name, &bytes, &[edit]);
        assert_eq!(inner_port(&m, "In2").as_deref(), Some("1"));
        assert_eq!(inner_port(&m, "In1").as_deref(), Some("2"));
        let dsts: Vec<_> = m.root.lines.iter().map(|l| l.dst.clone()).collect();
        assert_eq!(dsts[0], Some(ep("3", PortKind::In, 2)), "{name}");
        assert_eq!(dsts[1], Some(ep("3", PortKind::In, 1)), "{name}");
    }
}

#[test]
fn deleting_a_wired_port_block_needs_consent_and_updates_the_subsystem() {
    for (name, bytes) in files() {
        let delete = |disconnect| Edit::DeleteBlock {
            system: vec!["3".into()],
            id: "4".into(),
            disconnect,
        };
        assert!(unlinked_import::patch::apply_edits(
            name,
            &bytes,
            &[delete(DisconnectPolicy::Reject)]
        )
        .is_err());
        let m = roundtrip(name, &bytes, &[delete(DisconnectPolicy::Disconnect)]);
        assert_eq!(m.root.blocks[2].ports.inputs, 1, "{name}");
        assert_eq!(inner_port(&m, "In2").as_deref(), Some("1"));
        assert_eq!(m.root.lines.len(), 2, "{name}");
        assert_eq!(
            m.root.lines[0].dst,
            Some(ep("3", PortKind::In, 1)),
            "{name}"
        );
        // Removing the output removes the line it drove.
        let out = Edit::DeleteBlock {
            system: vec!["3".into()],
            id: "7".into(),
            disconnect: DisconnectPolicy::Disconnect,
        };
        let m = roundtrip(name, &bytes, &[out]);
        assert_eq!(m.root.blocks[2].ports.outputs, 0, "{name}");
        assert_eq!(m.root.lines.len(), 2, "{name}");
    }
}

#[test]
fn adding_a_port_block_adds_a_subsystem_port() {
    for (name, bytes) in files() {
        let model = unlinked_import::import(name, &bytes).unwrap();
        let sid = next_sid(&model).unwrap().to_string();
        let add = Edit::AddBlock {
            system: vec!["3".into()],
            id: sid.as_str().into(),
            block_type: "Inport".into(),
            name: "In3".into(),
            position: Rect::new(10.0, 110.0, 40.0, 125.0),
        };
        let wire = Edit::Connect {
            system: vec![],
            src: ep("2", PortKind::Out, 1),
            dst: ep("3", PortKind::In, 3),
        };
        let m = roundtrip(name, &bytes, &[add, wire]);
        assert_eq!(m.root.blocks[2].ports.inputs, 3, "{name}");
        assert_eq!(inner_port(&m, "In3").as_deref(), Some("3"));
    }
}
