//! Palette blocks that are Simulink library links persist as `Reference`
//! blocks with their `SourceBlock`, in both file formats.
use std::io::{Cursor, Write};
use unlinked_model::edit::Edit;
use unlinked_model::Rect;

fn add_wrap_to_zero(name: &str, bytes: &[u8]) {
    let add = Edit::AddBlock {
        system: vec![],
        id: "1".into(),
        block_type: "WrapToZero".into(),
        name: "wrap".into(),
        position: Rect::new(0.0, 0.0, 30.0, 30.0),
    };
    let out = unlinked_import::patch::apply_edits(name, bytes, &[add]).unwrap();
    let model = unlinked_import::import(name, &out).unwrap();
    let block = model.root.block(&"1".into()).unwrap();
    assert_eq!(block.block_type, "Reference");
    assert_eq!(
        block.library_source.as_deref(),
        Some("simulink/Discontinuities/Wrap To Zero")
    );
    assert_eq!(block.param("SourceType"), Some("Wrap To Zero"));
    assert_eq!(block.param("Threshold"), Some("255"));
    assert!(block.param("SourceBlock").is_none());
    assert_eq!((block.ports.inputs, block.ports.outputs), (1, 1));
    assert_eq!(
        unlinked_model::catalog::descriptor_for(block).map(|d| d.type_key),
        Some("WrapToZero")
    );
}

#[test]
fn library_link_persists_in_mdl() {
    add_wrap_to_zero("m.mdl", b"Model {\n Name \"m\"\n System {\n }\n}\n");
}

#[test]
fn library_link_persists_in_slx() {
    let src = r#"<ModelInformation><Model Name="m"><System></System></Model></ModelInformation>"#;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(src.as_bytes()).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    add_wrap_to_zero("m.slx", &bytes);
}

#[test]
fn multiline_library_names_survive_mdl_quoting() {
    let add = Edit::AddBlock {
        system: vec![],
        id: "1".into(),
        block_type: "SaturationDynamic".into(),
        name: "sat".into(),
        position: Rect::new(0.0, 0.0, 30.0, 50.0),
    };
    let out = unlinked_import::patch::apply_edits(
        "m.mdl",
        b"Model {\n Name \"m\"\n System {\n }\n}\n",
        &[add],
    )
    .unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(
        text.contains(r#"SourceBlock	"simulink/Discontinuities/Saturation\nDynamic""#),
        "{text}"
    );
    let model = unlinked_import::import("m.mdl", &out).unwrap();
    let block = model.root.block(&"1".into()).unwrap();
    assert_eq!(
        block.library_source.as_deref(),
        Some("simulink/Discontinuities/Saturation\nDynamic")
    );
    assert_eq!(block.param("SourceType"), Some("Saturation Dynamic"));
    assert_eq!((block.ports.inputs, block.ports.outputs), (3, 1));
}
