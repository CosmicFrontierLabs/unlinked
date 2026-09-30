//! Edits a user makes to a diagram.
//!
//! The same [`Edit`] values are applied to the in-memory IR (for an
//! immediate preview) and, by `unlinked-import`'s patcher, to the original
//! model file, so that everything the IR does not model survives a save.

use crate::{Block, BlockId, Branch, Endpoint, Line, Model, Rect, System};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Edit {
    /// Move and/or resize a block. Lines attached to it lose their stored
    /// vertices so they are routed afresh.
    MoveBlock {
        system: Vec<String>,
        id: BlockId,
        position: Rect,
    },
    /// Set a block parameter (dialog or mask parameter).
    SetParameter {
        system: Vec<String>,
        id: BlockId,
        name: String,
        value: String,
    },
    RenameBlock {
        system: Vec<String>,
        id: BlockId,
        name: String,
    },
    /// Delete a block and every line or branch connected to it.
    DeleteBlock { system: Vec<String>, id: BlockId },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("no subsystem at {0:?}")]
    NoSystem(Vec<String>),
    #[error("no block {0} in that system")]
    NoBlock(BlockId),
    #[error("a block named {0:?} already exists in that system")]
    DuplicateName(String),
    #[error("invalid value: {0}")]
    Invalid(String),
}

impl Edit {
    pub fn system(&self) -> &[String] {
        match self {
            Edit::MoveBlock { system, .. }
            | Edit::SetParameter { system, .. }
            | Edit::RenameBlock { system, .. }
            | Edit::DeleteBlock { system, .. } => system,
        }
    }

    pub fn block(&self) -> &BlockId {
        match self {
            Edit::MoveBlock { id, .. }
            | Edit::SetParameter { id, .. }
            | Edit::RenameBlock { id, .. }
            | Edit::DeleteBlock { id, .. } => id,
        }
    }

    /// Check an edit's values before applying it anywhere.
    pub fn validate(&self) -> Result<(), EditError> {
        match self {
            Edit::MoveBlock { position: p, .. } => {
                let finite = [p.left, p.top, p.right, p.bottom]
                    .iter()
                    .all(|v| v.is_finite());
                if !finite || p.right <= p.left || p.bottom <= p.top {
                    return Err(EditError::Invalid(
                        "block position must be a positive-size rectangle".into(),
                    ));
                }
            }
            Edit::SetParameter { name, value, .. } => {
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(EditError::Invalid(format!("bad parameter name {name:?}")));
                }
                if value.len() > 64 * 1024 {
                    return Err(EditError::Invalid("parameter value too long".into()));
                }
            }
            Edit::RenameBlock { name, .. } => {
                if name.trim().is_empty() || name.len() > 1024 {
                    return Err(EditError::Invalid(
                        "block names must be 1..=1024 bytes and not blank".into(),
                    ));
                }
            }
            Edit::DeleteBlock { .. } => {}
        }
        Ok(())
    }

    pub fn apply(&self, model: &mut Model) -> Result<(), EditError> {
        self.validate()?;
        let sys = system_mut(model, self.system())?;
        let id = self.block();
        let index = sys
            .blocks
            .iter()
            .position(|b| &b.id == id)
            .ok_or_else(|| EditError::NoBlock(id.clone()))?;
        match self {
            Edit::MoveBlock { position, .. } => {
                sys.blocks[index].position = *position;
                for line in sys.lines.iter_mut().filter(|l| touches(l, id)) {
                    clear_points(line);
                }
            }
            Edit::SetParameter { name, value, .. } => {
                set_parameter(&mut sys.blocks[index], name, value)
            }
            Edit::RenameBlock { name, .. } => {
                if sys.blocks.iter().any(|b| &b.name == name && &b.id != id) {
                    return Err(EditError::DuplicateName(name.clone()));
                }
                sys.blocks[index].name = name.clone();
            }
            Edit::DeleteBlock { .. } => {
                sys.blocks.remove(index);
                // Lines not attached to the block are left alone, even if
                // they were already dangling.
                sys.lines.retain_mut(|l| {
                    if !touches(l, id) {
                        return true;
                    }
                    if ends_at(&l.src, id) {
                        return false;
                    }
                    prune(&mut l.dst, &mut l.branches, id);
                    l.dst.is_some() || !l.branches.is_empty()
                });
            }
        }
        Ok(())
    }
}

fn system_mut<'a>(model: &'a mut Model, path: &[String]) -> Result<&'a mut System, EditError> {
    let mut sys = &mut model.root;
    for name in path {
        sys = sys
            .blocks
            .iter_mut()
            .find(|b| &b.name == name)
            .and_then(|b| b.subsystem.as_deref_mut())
            .ok_or_else(|| EditError::NoSystem(path.to_vec()))?;
    }
    Ok(sys)
}

/// Mask parameters take precedence, as in Simulink's dialog.
fn set_parameter(block: &mut Block, name: &str, value: &str) {
    if let Some(p) = block
        .mask
        .as_mut()
        .and_then(|m| m.parameters.iter_mut().find(|p| p.name == name))
    {
        p.value = value.to_string();
    } else {
        block.parameters.insert(name.to_string(), value.to_string());
    }
}

fn ends_at(ep: &Option<Endpoint>, id: &BlockId) -> bool {
    ep.as_ref().is_some_and(|e| &e.block == id)
}

/// Whether a line starts at, or any of its branches ends at, block `id`.
pub fn touches(line: &Line, id: &BlockId) -> bool {
    fn branches(bs: &[Branch], id: &BlockId) -> bool {
        bs.iter()
            .any(|b| ends_at(&b.dst, id) || branches(&b.branches, id))
    }
    ends_at(&line.src, id) || ends_at(&line.dst, id) || branches(&line.branches, id)
}

fn clear_points(line: &mut Line) {
    fn clear(bs: &mut [Branch]) {
        for b in bs {
            b.points.clear();
            clear(&mut b.branches);
        }
    }
    line.points.clear();
    clear(&mut line.branches);
}

/// Drop destinations at `id` and branches left with nowhere to go.
fn prune(dst: &mut Option<Endpoint>, branches: &mut Vec<Branch>, id: &BlockId) {
    if ends_at(dst, id) {
        *dst = None;
    }
    for b in branches.iter_mut() {
        prune(&mut b.dst, &mut b.branches, id);
    }
    branches.retain(|b| b.dst.is_some() || !b.branches.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use std::collections::BTreeMap;

    fn block(id: &str, name: &str) -> Block {
        Block {
            id: id.into(),
            block_type: "Gain".into(),
            name: name.into(),
            position: Rect::new(0.0, 0.0, 30.0, 30.0),
            orientation: Orientation::Right,
            mirrored: false,
            ports: PortCounts::from_slice(&[1, 1]),
            parameters: BTreeMap::new(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: BlockStyle::default(),
        }
    }

    fn ep(id: &str, kind: PortKind) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef { kind, index: 1 },
        }
    }

    fn model() -> Model {
        Model {
            name: "m".into(),
            source: SourceFormat::Slx,
            simulink_version: None,
            config: SimConfig::default(),
            root: System {
                blocks: vec![block("1", "a"), block("2", "b"), block("3", "c")],
                lines: vec![Line {
                    src: Some(ep("1", PortKind::Out)),
                    points: vec![Point::new(40.0, 15.0)],
                    branches: vec![
                        Branch {
                            dst: Some(ep("2", PortKind::In)),
                            ..Default::default()
                        },
                        Branch {
                            dst: Some(ep("3", PortKind::In)),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            },
            workspace: BTreeMap::new(),
        }
    }

    #[test]
    fn move_clears_attached_line_points() {
        let mut m = model();
        Edit::MoveBlock {
            system: vec![],
            id: "2".into(),
            position: Rect::new(100.0, 100.0, 130.0, 130.0),
        }
        .apply(&mut m)
        .unwrap();
        assert_eq!(m.root.blocks[1].position.left, 100.0);
        assert!(m.root.lines[0].points.is_empty());
    }

    #[test]
    fn delete_prunes_branches_and_orphan_lines() {
        let mut m = model();
        Edit::DeleteBlock {
            system: vec![],
            id: "2".into(),
        }
        .apply(&mut m)
        .unwrap();
        assert_eq!(m.root.blocks.len(), 2);
        assert_eq!(m.root.lines[0].branches.len(), 1);
        Edit::DeleteBlock {
            system: vec![],
            id: "1".into(),
        }
        .apply(&mut m)
        .unwrap();
        assert!(m.root.lines.is_empty());
    }

    #[test]
    fn rename_rejects_duplicates_and_set_parameter_prefers_mask() {
        let mut m = model();
        let dup = Edit::RenameBlock {
            system: vec![],
            id: "1".into(),
            name: "b".into(),
        };
        assert_eq!(dup.apply(&mut m), Err(EditError::DuplicateName("b".into())));
        m.root.blocks[0].mask = Some(Mask {
            parameters: vec![MaskParameter {
                name: "K".into(),
                value: "1".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        Edit::SetParameter {
            system: vec![],
            id: "1".into(),
            name: "K".into(),
            value: "5".into(),
        }
        .apply(&mut m)
        .unwrap();
        assert_eq!(
            m.root.blocks[0].mask.as_ref().unwrap().parameters[0].value,
            "5"
        );
        assert!(!m.root.blocks[0].parameters.contains_key("K"));
    }

    #[test]
    fn invalid_edits_are_rejected() {
        let mut m = model();
        let bad = Edit::MoveBlock {
            system: vec![],
            id: "1".into(),
            position: Rect::new(10.0, 10.0, 5.0, 20.0),
        };
        assert!(matches!(bad.apply(&mut m), Err(EditError::Invalid(_))));
        let missing = Edit::DeleteBlock {
            system: vec!["nope".into()],
            id: "1".into(),
        };
        assert!(matches!(missing.apply(&mut m), Err(EditError::NoSystem(_))));
    }
}
