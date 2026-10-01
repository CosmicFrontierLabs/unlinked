use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::{Endpoint, Point, PortKind, PortRef};
fn ep(id: &str, kind: PortKind) -> Endpoint {
    Endpoint {
        block: id.into(),
        port: PortRef { kind, index: 1 },
    }
}
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut mdl = String::from("Model {\n Name m\n System {\n");
    let mut xml = String::from("<ModelInformation><Model Name=\"m\"><System>");
    for (id, x) in [("1", 0), ("2", 100), ("3", 200)] {
        mdl+=&format!(" Block {{\n BlockType Gain\n Name b{id}\n SID {id}\n Ports [1, 1]\n Position [{x}, 0, {}, 30]\n }}\n",x+30);
        xml+=&format!("<Block BlockType=\"Gain\" Name=\"b{id}\" SID=\"{id}\"><P Name=\"Ports\">[1,1]</P><P Name=\"Position\">[{x},0,{},30]</P></Block>",x+30);
    }
    mdl+=" Line {\n SrcBlock b1\n SrcPort 1\n Points [20, 0]\n Branch {\n Points [0, 30]\n DstBlock b2\n DstPort 1\n }\n Branch {\n Points [20, 50]\n Branch {\n Points [10, 10]\n DstBlock b3\n DstPort 1\n }\n }\n }\n }\n}\n";
    xml+="<Line><P Name=\"Src\">1#out:1</P><P Name=\"Points\">[20,0]</P><Branch><P Name=\"Points\">[0,30]</P><P Name=\"Dst\">2#in:1</P></Branch><Branch><P Name=\"Points\">[20,50]</P><Branch><P Name=\"Points\">[10,10]</P><P Name=\"Dst\">3#in:1</P></Branch></Branch></Line></System></Model></ModelInformation>";
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    vec![
        ("m.mdl", mdl.into_bytes()),
        ("m.slx", zip.finish().unwrap().into_inner()),
    ]
}
#[test]
fn nested_leaf_and_trunk_routes_roundtrip_without_moving_other_vertices() {
    for (name, bytes) in fixtures() {
        let original = unlinked_import::import(name, &bytes).unwrap();
        let edits = vec![
            Edit::SetRoute {
                system: vec![],
                dst: ep("3", PortKind::In),
                points: vec![Point::new(80., 90.), Point::new(160., 90.)],
            },
            Edit::SetTrunkRoute {
                system: vec![],
                src: ep("1", PortKind::Out),
                points: vec![Point::new(70., 15.), Point::new(70., 40.)],
            },
        ];
        let mut expected = original.clone();
        apply_batch(&mut expected, &edits).unwrap();
        assert_eq!(
            expected.root.lines[0].branches[0],
            original.root.lines[0].branches[0]
        );
        assert_eq!(
            expected.root.lines[0].branches[1].points,
            original.root.lines[0].branches[1].points
        );
        let bytes = unlinked_import::patch::apply_edits(name, &bytes, &edits).unwrap();
        let actual = unlinked_import::import(name, &bytes).unwrap();
        assert_eq!(actual, expected, "{name}");
        let clear = [Edit::SetTrunkRoute {
            system: vec![],
            src: ep("1", PortKind::Out),
            points: vec![],
        }];
        apply_batch(&mut expected, &clear).unwrap();
        let bytes = unlinked_import::patch::apply_edits(name, &bytes, &clear).unwrap();
        assert_eq!(
            unlinked_import::import(name, &bytes).unwrap(),
            expected,
            "clear {name}"
        );
    }
}
#[test]
fn route_edits_fail_atomically_for_ambiguous_or_invalid_targets() {
    let (name, bytes) = fixtures().remove(0);
    let original = unlinked_import::import(name, &bytes).unwrap();
    for points in [
        vec![Point::new(f64::NAN, 0.)],
        vec![Point::new(0., 0.); 4097],
    ] {
        let mut model = original.clone();
        assert!(apply_batch(
            &mut model,
            &[Edit::SetRoute {
                system: vec![],
                dst: ep("2", PortKind::In),
                points
            }]
        )
        .is_err());
        assert_eq!(model, original);
    }
    let mut duplicate = original.clone();
    duplicate.root.lines.push(duplicate.root.lines[0].clone());
    assert!(Edit::SetTrunkRoute {
        system: vec![],
        src: ep("1", PortKind::Out),
        points: vec![]
    }
    .apply(&mut duplicate)
    .is_err());
    let mut ambiguous_block = original.clone();
    ambiguous_block
        .root
        .blocks
        .push(ambiguous_block.root.blocks[0].clone());
    assert!(Edit::SetTrunkRoute {
        system: vec![],
        src: ep("1", PortKind::Out),
        points: vec![]
    }
    .apply(&mut ambiguous_block)
    .is_err());
    let mut detached = original.clone();
    detached.root.lines[0].src = None;
    assert!(Edit::SetRoute {
        system: vec![],
        dst: ep("2", PortKind::In),
        points: vec![]
    }
    .apply(&mut detached)
    .is_err());
}
