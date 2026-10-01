use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, duplicate, next_sid};
use unlinked_model::{Point, PortKind};
#[test]
fn native_copy_preserves_parameters_and_internal_connections_in_both_formats() {
    let mdl=b"Model {\n Name m\n System {\n Block {\n BlockType Constant\n Name source\n SID 1\n Position [0,0,30,30]\n Value 7\n }\n Block {\n BlockType Gain\n Name gain\n SID 2\n Position [100,0,130,30]\n Gain 3\n }\n Line {\n SrcBlock source\n SrcPort 1\n DstBlock gain\n DstPort 1\n }\n }\n}\n";
    let xml = r#"<ModelInformation><Model Name="m"><System><Block BlockType="Constant" Name="source" SID="1"><P Name="Position">[0,0,30,30]</P><P Name="Value">7</P></Block><Block BlockType="Gain" Name="gain" SID="2"><P Name="Position">[100,0,130,30]</P><P Name="Gain">3</P></Block><Line><P Name="Src">1#out:1</P><P Name="Dst">2#in:1</P></Line></System></Model></ModelInformation>"#;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    for (name, bytes) in [
        ("m.mdl", mdl.to_vec()),
        ("m.slx", zip.finish().unwrap().into_inner()),
    ] {
        let bytes = unlinked_import::patch::apply_edits(
            name,
            &bytes,
            &[unlinked_model::edit::Edit::SetOrientation {
                system: vec![],
                id: "2".into(),
                orientation: unlinked_model::Orientation::Up,
                mirrored: true,
            }],
        )
        .unwrap();
        let model = unlinked_import::import(name, &bytes).unwrap();
        let edits = duplicate(
            &model,
            &vec![],
            &["1".into(), "2".into()],
            Point::new(0., 100.),
            next_sid(&model).unwrap(),
        )
        .unwrap();
        let mut expected = model.clone();
        apply_batch(&mut expected, &edits).unwrap();
        let out = unlinked_import::patch::apply_edits(name, &bytes, &edits).unwrap();
        let actual = unlinked_import::import(name, &out).unwrap();
        assert_eq!(actual, expected, "{name}");
        assert_eq!(actual.root.blocks.len(), 4);
        assert_eq!(actual.root.lines.len(), 2);
        assert_eq!(actual.root.blocks[2].param("Value"), Some("7"));
        assert_eq!(actual.root.blocks[3].param("Gain"), Some("3"));
        assert_eq!(
            actual.root.lines[1].src.as_ref().unwrap().port.kind,
            PortKind::Out
        );
    }
}
