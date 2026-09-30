//! Write edits back into the original model file.
//!
//! Edits are applied to a faithful representation of the file rather than
//! regenerated from the IR, so everything the IR does not model (masks,
//! configuration sets, Stateflow, styling, unknown elements) is preserved.

mod dom;
mod mdl;
mod slx;

use crate::{decode_text, import, ImportError};
use unlinked_model::edit::Edit;

/// Apply `edits` in order to the model in `bytes`, returning the new file.
///
/// Every edit is first checked against the imported IR, so an edit that
/// refers to a missing block or carries an invalid value is rejected before
/// anything is written.
pub fn apply_edits(filename: &str, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ImportError> {
    let mut model = import(filename, bytes)?;
    let mut named = Vec::with_capacity(edits.len());
    for edit in edits {
        let path: Vec<&str> = edit.system().iter().map(String::as_str).collect();
        let name = model
            .system_at(&path)
            .and_then(|s| s.block(edit.block()))
            .map(|b| b.name.clone())
            .ok_or_else(|| ImportError::Edit(format!("block {} not found", edit.block())))?;
        edit.apply(&mut model)
            .map_err(|e| ImportError::Edit(e.to_string()))?;
        named.push((edit.clone(), name));
    }

    if bytes.starts_with(b"PK\x03\x04") {
        return slx::apply(bytes, edits);
    }
    let utf8 = std::str::from_utf8(bytes).is_ok();
    let text = mdl::apply(&decode_text(bytes), &named)?;
    Ok(if utf8 {
        text.into_bytes()
    } else {
        encode_cp1252(&text)
    })
}

/// Inverse of the windows-1252 decoding used on import, for files that were
/// not UTF-8. Characters outside windows-1252 become `?`.
fn encode_cp1252(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| (0u8..=255).find(|&b| crate::cp1252(b) == c).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlinked_model::{BlockId, Rect};

    const MDL: &str = "Model {\n  Name\t\"m\"\n  System {\n    Name\t\"m\"\n    Block {\n      BlockType\tGain\n      Name\t\"g\"\n      Position\t[10, 10, 40, 40]\n    }\n  }\n}\n";

    fn gain_id() -> BlockId {
        import("m.mdl", MDL.as_bytes()).unwrap().root.blocks[0]
            .id
            .clone()
    }

    #[test]
    fn edits_are_validated_before_writing() {
        let bad = Edit::MoveBlock {
            system: vec![],
            id: "nope".into(),
            position: Rect::new(0.0, 0.0, 10.0, 10.0),
        };
        assert!(matches!(
            apply_edits("m.mdl", MDL.as_bytes(), &[bad]),
            Err(ImportError::Edit(_))
        ));
        let inverted = Edit::MoveBlock {
            system: vec![],
            id: gain_id(),
            position: Rect::new(10.0, 10.0, 0.0, 0.0),
        };
        assert!(apply_edits("m.mdl", MDL.as_bytes(), &[inverted]).is_err());
    }

    #[test]
    fn mdl_edit_reimports() {
        let out = apply_edits(
            "m.mdl",
            MDL.as_bytes(),
            &[
                Edit::SetParameter {
                    system: vec![],
                    id: gain_id(),
                    name: "Gain".into(),
                    value: "7".into(),
                },
                Edit::RenameBlock {
                    system: vec![],
                    id: gain_id(),
                    name: "k".into(),
                },
            ],
        )
        .unwrap();
        let model = import("m.mdl", &out).unwrap();
        assert_eq!(model.root.blocks[0].name, "k");
        assert_eq!(model.root.blocks[0].param("Gain"), Some("7"));
    }

    #[test]
    fn cp1252_roundtrips() {
        let bytes = b"\x80 caf\xe9 \x93q\x94";
        assert_eq!(encode_cp1252(&decode_text(bytes)), bytes);
    }
}
