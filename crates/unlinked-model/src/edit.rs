//! Edits a user makes to a diagram.
//!
//! The same [`Edit`] values are applied to the in-memory IR (for an
//! immediate preview) and, by `unlinked-import`'s patcher, to the original
//! model file, so that everything the IR does not model survives a save.
//!
//! Edits address systems and blocks by [`BlockId`] as imported from the
//! pinned base version, never by name: names change when blocks are
//! renamed, including earlier in the same batch.

use crate::{Block, BlockId, Branch, Chart, Endpoint, Line, Model, Rect, System};
use serde::{Deserialize, Serialize};

/// A diagram level: the IDs of the subsystem blocks from the root down.
/// The root system is the empty path.
pub type SystemRef = Vec<BlockId>;

/// What deleting a block does with lines attached to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectPolicy {
    /// Refuse to delete a block that has any line attached.
    Reject,
    /// Delete attached lines and branches along with the block.
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Edit {
    /// Move and/or resize a block. Lines attached to it lose their stored
    /// vertices so they are routed afresh.
    MoveBlock {
        system: SystemRef,
        id: BlockId,
        position: Rect,
    },
    /// Set a block parameter (dialog or mask parameter).
    SetParameter {
        system: SystemRef,
        id: BlockId,
        name: String,
        value: String,
    },
    RenameBlock {
        system: SystemRef,
        id: BlockId,
        name: String,
    },
    DeleteBlock {
        system: SystemRef,
        id: BlockId,
        disconnect: DisconnectPolicy,
    },
}

/// An edit that could not be applied, and its position in the batch.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("edit {index}: {error}")]
pub struct BatchError {
    pub index: usize,
    pub error: EditError,
}

/// Apply `edits` in order, all or nothing: on failure `model` is unchanged.
pub fn apply_batch(model: &mut Model, edits: &[Edit]) -> Result<(), BatchError> {
    let mut next = model.clone();
    for (index, edit) in edits.iter().enumerate() {
        edit.apply(&mut next)
            .map_err(|error| BatchError { index, error })?;
    }
    *model = next;
    Ok(())
}

/// Names of the subsystem blocks along `path`, as the model currently has
/// them.
pub fn system_names(model: &Model, path: &[BlockId]) -> Option<Vec<String>> {
    let mut sys = &model.root;
    let mut names = Vec::with_capacity(path.len());
    for id in path {
        let block = sys.blocks.iter().find(|b| &b.id == id)?;
        names.push(block.name.clone());
        sys = block.subsystem.as_deref()?;
    }
    Some(names)
}

/// IDs of the subsystem blocks along the name path `names`.
pub fn system_ids(model: &Model, names: &[String]) -> Option<SystemRef> {
    let mut sys = &model.root;
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let block = sys.blocks.iter().find(|b| &b.name == name)?;
        ids.push(block.id.clone());
        sys = block.subsystem.as_deref()?;
    }
    Some(ids)
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("no subsystem at {0:?}")]
    NoSystem(SystemRef),
    #[error("block {0} has connected lines")]
    Connected(BlockId),
    #[error("no block {0} in that system")]
    NoBlock(BlockId),
    #[error("a block named {0:?} already exists in that system")]
    DuplicateName(String),
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("{0:?} contains Stateflow charts, which cannot be renamed or deleted yet")]
    OwnsCharts(String),
}

impl Edit {
    pub fn system(&self) -> &[BlockId] {
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
        let charts = std::mem::take(&mut model.charts);
        let result = self.apply_to_diagram(model, &charts);
        model.charts = charts;
        result
    }

    fn apply_to_diagram(&self, model: &mut Model, charts: &[Chart]) -> Result<(), EditError> {
        let names = system_names(model, self.system())
            .ok_or_else(|| EditError::NoSystem(self.system().to_vec()))?;
        let sys = system_mut(model, self.system())?;
        let id = self.block();
        let index = sys
            .blocks
            .iter()
            .position(|b| &b.id == id)
            .ok_or_else(|| EditError::NoBlock(id.clone()))?;
        // Chart records are keyed by block path; renaming or deleting a
        // block at or above a chart would orphan them.
        if matches!(self, Edit::RenameBlock { .. } | Edit::DeleteBlock { .. }) {
            let mut prefix = names;
            prefix.push(sys.blocks[index].name.clone());
            if charts
                .iter()
                .any(|c| crate::stateflow::split_path(&c.name).starts_with(&prefix))
            {
                return Err(EditError::OwnsCharts(sys.blocks[index].name.clone()));
            }
        }
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
            Edit::DeleteBlock { disconnect, .. } => {
                if *disconnect == DisconnectPolicy::Reject
                    && sys.lines.iter().any(|l| touches(l, id))
                {
                    return Err(EditError::Connected(id.clone()));
                }
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

fn system_mut<'a>(model: &'a mut Model, path: &[BlockId]) -> Result<&'a mut System, EditError> {
    let mut sys = &mut model.root;
    for id in path {
        sys = sys
            .blocks
            .iter_mut()
            .find(|b| &b.id == id)
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
            interface: None,
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
            charts: Vec::new(),
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

    fn delete(id: &str, disconnect: DisconnectPolicy) -> Edit {
        Edit::DeleteBlock {
            system: vec![],
            id: id.into(),
            disconnect,
        }
    }

    #[test]
    fn delete_prunes_branches_and_orphan_lines() {
        let mut m = model();
        delete("2", DisconnectPolicy::Disconnect)
            .apply(&mut m)
            .unwrap();
        assert_eq!(m.root.blocks.len(), 2);
        assert_eq!(m.root.lines[0].branches.len(), 1);
        delete("1", DisconnectPolicy::Disconnect)
            .apply(&mut m)
            .unwrap();
        assert!(m.root.lines.is_empty());
    }

    #[test]
    fn delete_rejects_connected_blocks_unless_disconnecting() {
        let mut m = model();
        assert_eq!(
            delete("2", DisconnectPolicy::Reject).apply(&mut m),
            Err(EditError::Connected("2".into()))
        );
        assert_eq!(m.root.blocks.len(), 3);
        m.root.blocks.push(block("4", "free"));
        delete("4", DisconnectPolicy::Reject).apply(&mut m).unwrap();
    }

    #[test]
    fn batches_are_all_or_nothing() {
        let mut m = model();
        let before = m.clone();
        let batch = [
            Edit::RenameBlock {
                system: vec![],
                id: "1".into(),
                name: "renamed".into(),
            },
            delete("missing", DisconnectPolicy::Disconnect),
        ];
        let err = apply_batch(&mut m, &batch).unwrap_err();
        assert_eq!(err.index, 1);
        assert_eq!(m, before);
        apply_batch(&mut m, &batch[..1]).unwrap();
        assert_eq!(m.root.blocks[0].name, "renamed");
    }

    #[test]
    fn systems_are_addressed_by_id_across_renames() {
        let mut m = model();
        let mut sub = block("9", "Sub");
        sub.subsystem = Some(Box::new(System {
            blocks: vec![block("10", "inner")],
            ..Default::default()
        }));
        m.root.blocks.push(sub);
        let batch = [
            Edit::RenameBlock {
                system: vec![],
                id: "9".into(),
                name: "Renamed".into(),
            },
            Edit::RenameBlock {
                system: vec!["9".into()],
                id: "10".into(),
                name: "x".into(),
            },
        ];
        apply_batch(&mut m, &batch).unwrap();
        assert_eq!(system_names(&m, &["9".into()]).unwrap(), vec!["Renamed"]);
        assert_eq!(
            system_ids(&m, &["Renamed".into()]).unwrap(),
            vec![BlockId::from("9")]
        );
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
    fn chart_owners_cannot_be_renamed_or_deleted() {
        let mut m = model();
        let mut sub = block("9", "Sub");
        sub.subsystem = Some(Box::new(System {
            blocks: vec![block("10", "fcn")],
            ..Default::default()
        }));
        m.root.blocks.push(sub);
        m.charts.push(Chart {
            id: "1".into(),
            name: "Sub/fcn".into(),
            kind: ChartKind::MatlabFunction,
            states: vec![],
            transitions: vec![],
            junctions: vec![],
            data: vec![],
            script: None,
            update_method: None,
            sample_time: None,
        });
        let rename = |system: &[&str], id: &str| Edit::RenameBlock {
            system: system.iter().map(|s| BlockId::from(*s)).collect(),
            id: id.into(),
            name: "x".into(),
        };
        assert!(matches!(
            rename(&[], "9").apply(&mut m),
            Err(EditError::OwnsCharts(_))
        ));
        assert!(matches!(
            rename(&["9"], "10").apply(&mut m),
            Err(EditError::OwnsCharts(_))
        ));
        assert!(matches!(
            delete("9", DisconnectPolicy::Disconnect).apply(&mut m),
            Err(EditError::OwnsCharts(_))
        ));
        // Unrelated blocks and non-structural edits still work.
        rename(&[], "1").apply(&mut m).unwrap();
        Edit::MoveBlock {
            system: vec![],
            id: "9".into(),
            position: Rect::new(0.0, 0.0, 10.0, 10.0),
        }
        .apply(&mut m)
        .unwrap();
        assert_eq!(m.charts.len(), 1);
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
            disconnect: DisconnectPolicy::Disconnect,
        };
        assert!(matches!(missing.apply(&mut m), Err(EditError::NoSystem(_))));
    }
}
