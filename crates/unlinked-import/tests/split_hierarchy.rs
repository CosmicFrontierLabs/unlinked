//! New systems retain the package's split-system organization and OPC links.
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use unlinked_model::{
    edit::{apply_batch, Edit},
    Rect,
};

const ROOT: &str =
    "<ModelInformation><Model Name=\"m\"><System Ref=\"system_root\"/></Model></ModelInformation>";
const SYSTEM: &str = "<System><P Name=\"SIDHighWatermark\">2</P><Block BlockType=\"Constant\" Name=\"c\" SID=\"1\"><P Name=\"Position\">[0,0,30,30]</P><P Name=\"Value\">1</P></Block><Block BlockType=\"Gain\" Name=\"g\" SID=\"2\"><P Name=\"Position\">[100,0,130,30]</P><P Name=\"Gain\">2</P></Block><Line><P Name=\"Src\">1#out:1</P><P Name=\"Dst\">2#in:1</P></Line></System>";
const TYPES: &str = "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/vnd.mathworks.simulink.mdl+xml\"/><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/></Types>";
const RELS: &str = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"system_10_1\" Target=\"external.dat\" Type=\"opaque\"/></Relationships>";
fn package(extra: bool) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut entries = vec![
        ("simulink/blockdiagram.xml", ROOT),
        ("simulink/systems/system_root.xml", SYSTEM),
        ("[Content_Types].xml", TYPES),
        ("opaque.dat", "keep exactly"),
    ];
    if extra {
        entries.extend([
            ("simulink/systems/system_10.xml", "<System/>"),
            ("simulink/systems/_rels/system_root.xml.rels", RELS),
        ]);
    }
    for (name, text) in entries {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(text.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}
fn entries(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut text = String::new();
            f.read_to_string(&mut text).unwrap();
            (name, text)
        })
        .collect()
}
fn create() -> Edit {
    Edit::CreateSubsystem {
        system: vec![],
        ids: vec!["2".into()],
        id: "10".into(),
        name: "controller".into(),
    }
}
#[test]
fn creates_collision_safe_part_with_relationship_and_content_type() {
    let before = package(true);
    let mut expected = unlinked_import::import("m.slx", &before).unwrap();
    let edits = vec![
        create(),
        Edit::SetParameter {
            system: vec!["10".into()],
            id: "2".into(),
            name: "Gain".into(),
            value: "3".into(),
        },
    ];
    apply_batch(&mut expected, &edits).unwrap();
    let after = unlinked_import::patch::apply_edits("m.slx", &before, &edits).unwrap();
    assert_eq!(unlinked_import::import("m.slx", &after).unwrap(), expected);
    let a = entries(&after);
    let b = entries(&before);
    assert_eq!(
        a["simulink/blockdiagram.xml"],
        b["simulink/blockdiagram.xml"]
    );
    assert_eq!(a["opaque.dat"], b["opaque.dat"]);
    assert_eq!(
        a["simulink/systems/system_10.xml"],
        b["simulink/systems/system_10.xml"]
    );
    assert!(a["simulink/systems/system_root.xml"].contains("<System Ref=\"system_10_2\"/>"));
    assert!(a["simulink/systems/system_10_2.xml"].contains("Name=\"Gain\">3</P>"));
    let rel = &a["simulink/systems/_rels/system_root.xml.rels"];
    assert!(rel.contains("Id=\"system_10_2\""));
    assert!(rel.contains("Target=\"system_10_2.xml\""));
    assert!(rel.contains("http://schemas.mathworks.com/simulink/2010/relationships/system"));
    assert!(rel.contains("Target=\"external.dat\""));
    assert!(a["[Content_Types].xml"].contains("PartName=\"/simulink/systems/system_10_2.xml\""));
}
#[test]
fn new_relationship_part_and_nested_creation_resolve_in_same_batch() {
    let before = package(false);
    let edits = vec![
        create(),
        Edit::CreateSubsystem {
            system: vec!["10".into()],
            ids: vec!["2".into()],
            id: "20".into(),
            name: "inner".into(),
        },
        Edit::MoveBlock {
            system: vec!["10".into(), "20".into()],
            id: "2".into(),
            position: Rect::new(200., 0., 230., 30.),
        },
    ];
    let mut expected = unlinked_import::import("m.slx", &before).unwrap();
    apply_batch(&mut expected, &edits).unwrap();
    let after = unlinked_import::patch::apply_edits("m.slx", &before, &edits).unwrap();
    assert_eq!(unlinked_import::import("m.slx", &after).unwrap(), expected);
    let a = entries(&after);
    assert!(a["simulink/systems/system_10.xml"].contains("Ref=\"system_20\""));
    assert!(a["simulink/systems/_rels/system_10.xml.rels"].contains("Target=\"system_20.xml\""));
    assert!(a.contains_key("simulink/systems/system_20.xml"));
}

#[test]
fn refuses_moving_opaque_part_relative_references() {
    let original = package(true);
    let mut parts = entries(&original);
    let root = parts.get_mut("simulink/systems/system_root.xml").unwrap();
    *root = root.replace(
        "<P Name=\"Gain\">2</P>",
        "<P Name=\"Gain\">2</P><Unknown Ref=\"opaque_payload\"/>",
    );
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in parts {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(text.as_bytes()).unwrap();
    }
    let input = z.finish().unwrap().into_inner();
    let err = unlinked_import::patch::apply_edits("m.slx", &input, &[create()]).unwrap_err();
    assert!(err.to_string().contains("part-relative"), "{err}");
}
