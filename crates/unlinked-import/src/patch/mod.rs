//! Write edits back into the original model file.
//!
//! Edits are applied to a faithful representation of the file rather than
//! regenerated from the IR, so everything the IR does not model (masks,
//! configuration sets, Stateflow, styling, unknown elements) is preserved.

mod dom;
mod mdl;
mod slx;

use crate::{decode_text, import, ImportError};
use std::collections::HashMap;
use unlinked_model::catalog::interface_port_number;
use unlinked_model::edit::{structural_regression, system_names, Edit};
use unlinked_model::{Block, BlockId, PortCounts, PortKind};

/// An edit with the names its targets have in the file at that point of
/// the batch, and what it produced in the IR.
struct Resolved {
    edit: Edit,
    /// Subsystem block names from the root.
    system: Vec<String>,
    /// Names of the system's blocks before the edit.
    names: HashMap<BlockId, String>,
    /// The block an `AddBlock` created.
    added: Option<Block>,
    /// New port counts of the edited block, when the edit changed them.
    ports: Option<PortCounts>,
    /// New port numbers of other port blocks in the system: their `Port`
    /// and, for bus elements, the interface `PortNumber`.
    renumbered: Vec<(BlockId, String)>,
}

impl Resolved {
    /// Current name of block `id` in the edited system.
    fn name(&self, id: &BlockId) -> Result<&str, ImportError> {
        self.names
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| ImportError::Edit(format!("block {id} not found")))
    }
}

/// Apply `edits` in order to the model in `bytes`, returning the new file.
///
/// Every edit is first checked against the imported IR, so an edit that
/// refers to a missing block, carries an invalid value or adds a structural
/// error is rejected before anything is written.
pub fn apply_edits(filename: &str, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ImportError> {
    let original = import(filename, bytes)?;
    let mut model = original.clone();
    let mut resolved = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        // Files locate systems and blocks by name, so resolve the edit's
        // IDs against the model as earlier edits in the batch left it.
        let failed = |message: String| ImportError::Edit(format!("edit {index}: {message}"));
        let system = system_names(&model, edit.system())
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let path: Vec<&str> = system.iter().map(String::as_str).collect();
        let sys = model
            .system_at(&path)
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let names = sys
            .blocks
            .iter()
            .map(|b| (b.id.clone(), b.name.clone()))
            .collect();
        let ports_before = edit.block().and_then(|id| sys.block(id)).map(|b| b.ports);
        // Deleting a port block renumbers its siblings.
        let ports_numbered: Vec<(BlockId, Option<u32>)> = match edit {
            Edit::DeleteBlock { .. } => sys
                .blocks
                .iter()
                .map(|b| (b.id.clone(), interface_port_number(b).ok()))
                .collect(),
            _ => Vec::new(),
        };
        edit.apply(&mut model).map_err(|e| failed(e.to_string()))?;
        let sys = system_names(&model, edit.system())
            .and_then(|names| {
                let path: Vec<&str> = names.iter().map(String::as_str).collect();
                model.system_at(&path)
            })
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let added = match edit {
            Edit::AddBlock { id, .. } => sys.block(id).cloned(),
            _ => None,
        };
        let ports = edit
            .block()
            .and_then(|id| sys.block(id))
            .map(|b| b.ports)
            .filter(|p| ports_before.is_some_and(|before| before != *p));
        let renumbered = ports_numbered
            .into_iter()
            .filter_map(|(id, before)| {
                let now = interface_port_number(sys.block(&id)?).ok()?;
                (before != Some(now)).then(|| (id, now.to_string()))
            })
            .collect();
        resolved.push(Resolved {
            edit: edit.clone(),
            system,
            names,
            added,
            ports,
            renumbered,
        });
    }
    structural_regression(&original, &model).map_err(|e| ImportError::Edit(e.to_string()))?;

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

/// The `Ports` value for `p`: counts in Simulink's order, trailing zeros
/// dropped.
fn format_ports(p: &PortCounts) -> String {
    let mut v = [
        p.inputs, p.outputs, p.enable, p.trigger, p.state, p.lconn, p.rconn, p.ifaction, p.reset,
    ]
    .to_vec();
    while v.last() == Some(&0) {
        v.pop();
    }
    let parts: Vec<String> = v.iter().map(u32::to_string).collect();
    format!("[{}]", parts.join(", "))
}

/// Parse an SLX-style endpoint `12#out:1` into its SID and port.
fn parse_endpoint(v: &str, default_kind: PortKind) -> Option<(&str, unlinked_model::PortRef)> {
    let (sid, port) = v.split_once('#')?;
    Some((sid, crate::convert::parse_port(port, default_kind)?))
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
    fn structural_edits_write_simulink_sections() {
        use unlinked_model::edit::{apply_batch, next_sid};
        use unlinked_model::{Endpoint, PortRef};
        let src = MDL.replace(
            "Name\t\"m\"\n    Block",
            "Name\t\"m\"\n    SIDHighWatermark\t\"7\"\n    Block",
        );
        let model = import("m.mdl", src.as_bytes()).unwrap();
        let sid = next_sid(&model).unwrap().to_string();
        assert_eq!(sid, "8");
        let port = |block: &BlockId, kind, index| Endpoint {
            block: block.clone(),
            port: PortRef { kind, index },
        };
        let edits = [
            Edit::AddBlock {
                system: vec![],
                id: sid.as_str().into(),
                block_type: "Sum".into(),
                name: "add".into(),
                position: Rect::new(100.0, 10.0, 130.0, 40.0),
            },
            Edit::Connect {
                system: vec![],
                src: port(&gain_id(), PortKind::Out, 1),
                dst: port(&sid.as_str().into(), PortKind::In, 2),
            },
        ];
        let out = apply_edits("m.mdl", src.as_bytes(), &edits).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.contains(
            "    Block {\n      BlockType\tSum\n      Name\t\"add\"\n      SID\t\"8\"\n      Ports\t[2, 1]\n"
        ), "{text}");
        assert!(text.contains(
            "    Line {\n      SrcBlock\t\"g\"\n      SrcPort\t1\n      DstBlock\t\"add\"\n      DstPort\t2\n    }\n"
        ), "{text}");
        assert!(text.contains("SIDHighWatermark\t\"8\""), "{text}");
        let mut expected = model;
        apply_batch(&mut expected, &edits).unwrap();
        assert_eq!(import("m.mdl", &out).unwrap().root, expected.root);

        // Disconnecting a line's only destination removes the line.
        let more = [Edit::Disconnect {
            system: vec![],
            dst: port(&sid.as_str().into(), PortKind::In, 2),
        }];
        let out = apply_edits("m.mdl", &out, &more).unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("Line {"));
    }

    #[test]
    fn cp1252_roundtrips() {
        let bytes = b"\x80 caf\xe9 \x93q\x94";
        assert_eq!(encode_cp1252(&decode_text(bytes)).unwrap(), bytes);
        assert!(matches!(encode_cp1252("漢字"), Err(ImportError::Edit(_))));
    }
}
