//! Library blocks added from the palette are `Reference` blocks linked to
//! their `SourceBlock`. The MDL and SLX patchers must write that link and
//! the mask parameters, or the saved file would hold a `Reference` block
//! Simulink cannot resolve and the registry would not recognise on re-import.

use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::{catalog, Rect};

/// SIDs and palette keys of the blocks added; the Repeating Sequence path
/// wraps onto two lines in files.
const ADDED: &[(&str, &str)] = &[
    ("1", "Ramp"),
    ("2", "RepeatingSequence"),
    ("3", "ChirpSignal"),
];

fn edits() -> Vec<Edit> {
    let mut edits: Vec<Edit> = ADDED
        .iter()
        .map(|(id, key)| Edit::AddBlock {
            system: vec![],
            id: (*id).into(),
            block_type: (*key).into(),
            name: (*key).into(),
            position: Rect::new(0.0, 0.0, 30.0, 30.0),
        })
        .collect();
    // A dialog edit of a mask parameter on a new library block.
    edits.push(Edit::SetParameter {
        system: vec![],
        id: "1".into(),
        name: "slope".into(),
        value: "2".into(),
    });
    edits
}

fn library_blocks_roundtrip(name: &str, bytes: &[u8]) {
    let edits = edits();
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    apply_batch(&mut expected, &edits).unwrap();
    let patched = unlinked_import::patch::apply_edits(name, bytes, &edits).unwrap();
    let actual = unlinked_import::import(name, &patched).unwrap();
    assert_eq!(actual.root.blocks, expected.root.blocks);
    for (id, key) in ADDED {
        let block = actual
            .root
            .blocks
            .iter()
            .find(|b| b.id.0 == *id)
            .unwrap_or_else(|| panic!("{key} missing after re-import"));
        let descriptor = catalog::find(key).unwrap();
        assert_eq!(block.block_type, "Reference");
        assert_eq!(block.library_source.as_deref(), descriptor.source_block);
        assert_eq!(
            catalog::descriptor_for(block).map(|d| d.type_key),
            Some(*key)
        );
        for parameter in descriptor.parameters {
            assert!(
                block.parameters.contains_key(parameter.name),
                "{key} lost {}",
                parameter.name
            );
        }
    }
    assert_eq!(actual.root.blocks[0].param("slope"), Some("2"));
}

#[test]
fn library_blocks_roundtrip_mdl() {
    library_blocks_roundtrip("m.mdl", b"Model {\n Name \"m\"\n System {\n }\n}\n");
}

#[test]
fn library_blocks_roundtrip_slx() {
    let src = r#"<ModelInformation><Model Name="m"><System></System></Model></ModelInformation>"#;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(src.as_bytes()).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    library_blocks_roundtrip("m.slx", &bytes);
}
