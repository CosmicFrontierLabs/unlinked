//! Structural comparison of two versions of a model.
//!
//! Blocks are matched per diagram level by id (the Simulink SID), falling
//! back to the block name for ids synthesized from paths. Matched
//! subsystems are compared recursively; an added or removed subsystem is
//! reported once, not per contained block.

use crate::{Block, BlockId, Connection, Model, System};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlockChange {
    Added,
    Removed,
    Modified(Modification),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Modification {
    pub renamed_from: Option<String>,
    pub type_changed_from: Option<String>,
    pub moved: bool,
    pub resized: bool,
    /// Parameters as `(name, old, new)`; `None` means absent.
    pub parameters: Vec<(String, Option<String>, Option<String>)>,
    pub mask_changed: bool,
}

impl Modification {
    fn is_empty(&self) -> bool {
        *self == Modification::default()
    }

    /// Only the layout changed.
    pub fn layout_only(&self) -> bool {
        (self.moved || self.resized)
            && self.renamed_from.is_none()
            && self.type_changed_from.is_none()
            && self.parameters.is_empty()
            && !self.mask_changed
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDiff {
    /// Block names from the root down to the containing system.
    pub system: Vec<String>,
    pub id: BlockId,
    pub name: String,
    pub block_type: String,
    pub change: BlockChange,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemDiff {
    pub system: Vec<String>,
    pub connections_added: Vec<Connection>,
    pub connections_removed: Vec<Connection>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelDiff {
    pub blocks: Vec<BlockDiff>,
    /// Systems whose wiring changed.
    pub systems: Vec<SystemDiff>,
    /// Solver configuration as `(name, old, new)`.
    pub config: Vec<(String, Option<String>, Option<String>)>,
}

impl ModelDiff {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.systems.is_empty() && self.config.is_empty()
    }

    /// Changes to blocks directly inside the system at `path`.
    pub fn in_system<'a>(&'a self, path: &'a [String]) -> impl Iterator<Item = &'a BlockDiff> + 'a {
        self.blocks.iter().filter(move |b| b.system == path)
    }

    /// Whether anything changed at or below the system at `path`.
    pub fn touches(&self, path: &[String]) -> bool {
        self.blocks.iter().any(|b| b.system.starts_with(path))
            || self.systems.iter().any(|s| s.system.starts_with(path))
    }
}

/// Parameters that only record editor state and are not model changes.
const IGNORED_PARAMETERS: &[&str] = &["ZOrder", "SIDHighWatermark"];

fn match_key(b: &Block) -> String {
    if b.id.0.starts_with("path:") {
        format!("name:{}", b.name)
    } else {
        format!("id:{}", b.id.0)
    }
}

fn map_diff(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Vec<(String, Option<String>, Option<String>)> {
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    keys.into_iter()
        .filter(|k| !IGNORED_PARAMETERS.contains(&k.as_str()))
        .filter_map(|k| {
            let (a, b) = (old.get(k), new.get(k));
            (a != b).then(|| (k.clone(), a.cloned(), b.cloned()))
        })
        .collect()
}

fn compare_block(old: &Block, new: &Block) -> Modification {
    let p = |b: &Block| (b.position.left, b.position.top);
    let s = |b: &Block| (b.position.width(), b.position.height());
    Modification {
        renamed_from: (old.name != new.name).then(|| old.name.clone()),
        type_changed_from: (old.block_type != new.block_type).then(|| old.block_type.clone()),
        moved: p(old) != p(new),
        resized: s(old) != s(new),
        parameters: map_diff(&old.parameters, &new.parameters),
        mask_changed: old.mask != new.mask,
    }
}

fn connections(sys: &System) -> BTreeSet<Connection> {
    sys.connections().into_iter().collect()
}

fn diff_system(old: &System, new: &System, path: &[String], out: &mut ModelDiff) {
    let old_by: BTreeMap<String, &Block> = old.blocks.iter().map(|b| (match_key(b), b)).collect();
    let new_by: BTreeMap<String, &Block> = new.blocks.iter().map(|b| (match_key(b), b)).collect();

    let entry = |b: &Block, change| BlockDiff {
        system: path.to_vec(),
        id: b.id.clone(),
        name: b.name.clone(),
        block_type: b.block_type.clone(),
        change,
    };

    for (key, nb) in &new_by {
        match old_by.get(key) {
            None => out.blocks.push(entry(nb, BlockChange::Added)),
            Some(ob) => {
                let m = compare_block(ob, nb);
                if !m.is_empty() {
                    out.blocks.push(entry(nb, BlockChange::Modified(m)));
                }
                if let (Some(os), Some(ns)) = (&ob.subsystem, &nb.subsystem) {
                    let mut sub = path.to_vec();
                    sub.push(nb.name.clone());
                    diff_system(os, ns, &sub, out);
                }
            }
        }
    }
    for (key, ob) in &old_by {
        if !new_by.contains_key(key) {
            out.blocks.push(entry(ob, BlockChange::Removed));
        }
    }

    let (oc, nc) = (connections(old), connections(new));
    let added: Vec<Connection> = nc.difference(&oc).cloned().collect();
    let removed: Vec<Connection> = oc.difference(&nc).cloned().collect();
    if !added.is_empty() || !removed.is_empty() {
        out.systems.push(SystemDiff {
            system: path.to_vec(),
            connections_added: added,
            connections_removed: removed,
        });
    }
}

pub fn diff(old: &Model, new: &Model) -> ModelDiff {
    let mut out = ModelDiff::default();
    diff_system(&old.root, &new.root, &[], &mut out);
    out.config = map_diff(&old.config.raw, &new.config.raw);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn block(id: &str, name: &str, x: f64) -> Block {
        Block {
            id: id.into(),
            block_type: "Gain".into(),
            name: name.into(),
            position: Rect::new(x, 0.0, x + 30.0, 30.0),
            orientation: Orientation::Right,
            mirrored: false,
            ports: PortCounts::from_slice(&[1, 1]),
            parameters: BTreeMap::from([("Gain".to_string(), "1".to_string())]),
            mask: None,
            library_source: None,
            subsystem: None,
            style: BlockStyle::default(),
        }
    }

    fn model(root: System) -> Model {
        Model {
            name: "m".into(),
            source: SourceFormat::Slx,
            simulink_version: None,
            config: SimConfig::default(),
            root,
            workspace: BTreeMap::new(),
        }
    }

    #[test]
    fn detects_added_removed_modified_and_rewired() {
        let old = model(System {
            blocks: vec![block("1", "a", 0.0), block("2", "b", 50.0)],
            lines: vec![Line {
                src: Some(Endpoint {
                    block: "1".into(),
                    port: PortRef {
                        kind: PortKind::Out,
                        index: 1,
                    },
                }),
                dst: Some(Endpoint {
                    block: "2".into(),
                    port: PortRef {
                        kind: PortKind::In,
                        index: 1,
                    },
                }),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut changed = block("1", "a", 10.0);
        changed.parameters.insert("Gain".into(), "5".into());
        changed.parameters.insert("ZOrder".into(), "9".into());
        let new = model(System {
            blocks: vec![changed, block("3", "c", 100.0)],
            ..Default::default()
        });
        let d = diff(&old, &new);
        let by_name: BTreeMap<&str, &BlockChange> = d
            .blocks
            .iter()
            .map(|b| (b.name.as_str(), &b.change))
            .collect();
        assert_eq!(by_name["c"], &BlockChange::Added);
        assert_eq!(by_name["b"], &BlockChange::Removed);
        let BlockChange::Modified(m) = by_name["a"] else {
            panic!()
        };
        assert!(m.moved && !m.resized);
        assert_eq!(
            m.parameters,
            vec![("Gain".into(), Some("1".into()), Some("5".into()))]
        );
        assert_eq!(d.systems.len(), 1);
        assert_eq!(d.systems[0].connections_removed.len(), 1);
    }

    #[test]
    fn identical_models_have_no_diff() {
        let m = model(System {
            blocks: vec![block("1", "a", 0.0)],
            ..Default::default()
        });
        assert!(diff(&m, &m).is_empty());
    }

    #[test]
    fn nested_changes_report_their_system() {
        let mut outer_old = block("1", "Sub", 0.0);
        outer_old.subsystem = Some(Box::new(System {
            blocks: vec![block("2", "inner", 0.0)],
            ..Default::default()
        }));
        let mut outer_new = outer_old.clone();
        outer_new.subsystem.as_mut().unwrap().blocks[0]
            .parameters
            .insert("Gain".into(), "3".into());
        let d = diff(
            &model(System {
                blocks: vec![outer_old],
                ..Default::default()
            }),
            &model(System {
                blocks: vec![outer_new],
                ..Default::default()
            }),
        );
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].system, vec!["Sub".to_string()]);
        assert!(d.touches(&["Sub".to_string()]));
        assert!(d.touches(&[]));
    }
}
