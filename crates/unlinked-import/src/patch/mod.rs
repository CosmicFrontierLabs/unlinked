//! Write edits back into the original model file.
//!
//! Edits are applied to a faithful representation of the file rather than
//! regenerated from the IR, so everything the IR does not model (masks,
//! configuration sets, Stateflow, styling, unknown elements) is preserved.

mod dom;
mod mdl;
mod slx;

use crate::{decode_text, import, ImportError};
use unlinked_model::edit::{system_names, Edit};

/// An edit with the names its targets have in the file at that point of
/// the batch.
struct Resolved {
    edit: Edit,
    /// Subsystem block names from the root.
    system: Vec<String>,
    /// Name of the edited block.
    block: String,
}

/// Apply `edits` in order to the model in `bytes`, returning the new file.
///
/// Every edit is first checked against the imported IR, so an edit that
/// refers to a missing block or carries an invalid value is rejected before
/// anything is written.
pub fn apply_edits(filename: &str, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ImportError> {
    let mut model = import(filename, bytes)?;
    let mut resolved = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        // Files locate systems and blocks by name, so resolve the edit's
        // IDs against the model as earlier edits in the batch left it.
        let failed = |message: String| ImportError::Edit(format!("edit {index}: {message}"));
        let system = system_names(&model, edit.system())
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let path: Vec<&str> = system.iter().map(String::as_str).collect();
        let block = model
            .system_at(&path)
            .and_then(|s| s.block(edit.block()))
            .map(|b| b.name.clone())
            .ok_or_else(|| failed(format!("block {} not found", edit.block())))?;
        edit.apply(&mut model).map_err(|e| failed(e.to_string()))?;
        resolved.push(Resolved {
            edit: edit.clone(),
            system,
            block,
        });
    }

    if bytes.starts_with(b"PK\x03\x04") {
        return slx::apply(bytes, &resolved);
    }
    let utf8 = std::str::from_utf8(bytes).is_ok();
    let text = mdl::apply(&decode_text(bytes), &resolved)?;
    if utf8 {
        Ok(text.into_bytes())
    } else {
        encode_cp1252(&text)
    }
}

/// Inverse of the windows-1252 decoding used on import, for files that were
/// not UTF-8. Characters windows-1252 cannot represent are rejected.
fn encode_cp1252(text: &str) -> Result<Vec<u8>, ImportError> {
    text.chars()
        .map(|c| {
            (0u8..=255).find(|&b| crate::cp1252(b) == c).ok_or_else(|| {
                ImportError::Edit(format!(
                    "{c:?} cannot be stored in this windows-1252 encoded file"
                ))
            })
        })
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
        assert_eq!(encode_cp1252(&decode_text(bytes)).unwrap(), bytes);
        assert!(matches!(encode_cp1252("漢字"), Err(ImportError::Edit(_))));
    }
}
