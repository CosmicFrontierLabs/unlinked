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

/// A `(name, old, new)` change; `None` means absent.
pub type Change = (String, Option<String>, Option<String>);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Modification {
    pub renamed_from: Option<String>,
    pub type_changed_from: Option<String>,
    pub moved: bool,
    pub resized: bool,
    /// Orientation, mirroring or colors/font/name placement changed.
    pub appearance_changed: bool,
    pub parameters: Vec<Change>,
    pub mask_changed: bool,
    pub ports_changed: bool,
    /// Library link `(old, new)`.
    pub library_changed: Option<(Option<String>, Option<String>)>,
    /// The block gained or lost its contained system.
    pub subsystem_changed: bool,
    /// Bus element port interface properties (`PortNumber`, `Element`, ...).
    #[serde(default)]
    pub interface: Vec<Change>,
}

impl Modification {
    fn is_empty(&self) -> bool {
        *self == Modification::default()
    }

    /// Only layout or appearance changed, not behavior.
    pub fn layout_only(&self) -> bool {
        (self.moved || self.resized || self.appearance_changed)
            && self.renamed_from.is_none()
            && self.type_changed_from.is_none()
            && self.parameters.is_empty()
            && !self.mask_changed
            && !self.ports_changed
            && self.library_changed.is_none()
            && !self.subsystem_changed
            && self.interface.is_empty()
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChartChange {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartDiff {
    pub id: String,
    pub name: String,
    pub previous_name: Option<String>,
    pub change: ChartChange,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelDiff {
    /// Chart code, graphical state, data, and timing changes.
    #[serde(default)]
    pub charts: Vec<ChartDiff>,
    pub blocks: Vec<BlockDiff>,
    /// Systems whose wiring changed.
    pub systems: Vec<SystemDiff>,
    /// Solver configuration.
    pub config: Vec<Change>,
    /// Model workspace variables.
    pub workspace: Vec<Change>,
}

impl ModelDiff {
    pub fn is_empty(&self) -> bool {
        self.charts.is_empty()
            && self.blocks.is_empty()
            && self.systems.is_empty()
            && self.config.is_empty()
            && self.workspace.is_empty()
    }

    /// Changes to blocks directly inside the system at `path`.
    pub fn in_system<'a>(&'a self, path: &'a [String]) -> impl Iterator<Item = &'a BlockDiff> + 'a {
        self.blocks.iter().filter(move |b| b.system == path)
    }

    /// Whether anything changed at or below the system at `path`.
    pub fn touches(&self, path: &[String]) -> bool {
        self.blocks.iter().any(|b| b.system.starts_with(path))
            || self.systems.iter().any(|s| s.system.starts_with(path))
            || self.charts.iter().any(|c| {
                crate::stateflow::split_path(&c.name).starts_with(path)
                    || c.previous_name
                        .as_deref()
                        .is_some_and(|name| crate::stateflow::split_path(name).starts_with(path))
            })
    }
}

/// Parameters that only record editor state and are not model changes.
const IGNORED_PARAMETERS: &[&str] = &["ZOrder", "SIDHighWatermark"];

/// Stable per-level keys: the SID, or the name for ids synthesized from
/// paths (those change when a parent is renamed). Repeated keys, which a
/// valid model never has, get an occurrence suffix instead of colliding.
fn keyed(sys: &System) -> (BTreeMap<String, &Block>, BTreeMap<&BlockId, String>) {
    let mut by_key = BTreeMap::new();
    let mut key_of = BTreeMap::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for b in &sys.blocks {
        let base = if b.id.0.starts_with("path:") {
            format!("name:{}", b.name)
        } else {
            format!("id:{}", b.id.0)
        };
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { base } else { format!("{base}#{n}") };
        key_of.insert(&b.id, key.clone());
        by_key.insert(key, b);
    }
    (by_key, key_of)
}

/// Keys whose values differ. `ignored` names are skipped (editor noise in
/// block parameters; nothing for workspace or configuration maps).
fn map_diff(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
    ignored: &[&str],
) -> Vec<Change> {
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    keys.into_iter()
        .filter(|k| !ignored.contains(&k.as_str()))
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
        appearance_changed: old.orientation != new.orientation
            || old.mirrored != new.mirrored
            || old.style != new.style,
        parameters: map_diff(&old.parameters, &new.parameters, IGNORED_PARAMETERS),
        mask_changed: old.mask != new.mask,
        ports_changed: old.ports != new.ports,
        library_changed: (old.library_source != new.library_source)
            .then(|| (old.library_source.clone(), new.library_source.clone())),
        subsystem_changed: old.subsystem.is_some() != new.subsystem.is_some(),
        interface: {
            let raw = |b: &Block| {
                b.interface
                    .as_ref()
                    .map(|i| i.raw.clone())
                    .unwrap_or_default()
            };
            map_diff(&raw(old), &raw(new), &[])
        },
    }
}

/// Connections with endpoints expressed as stable block keys, mapped to a
/// representative connection for reporting.
fn connections(sys: &System, key_of: &BTreeMap<&BlockId, String>) -> BTreeMap<String, Connection> {
    let key = |id: &BlockId| {
        key_of
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("missing:{id}"))
    };
    sys.connections()
        .into_iter()
        .map(|c| {
            let k = format!(
                "{}|{:?}|{}|{:?}",
                key(&c.src.block),
                c.src.port,
                key(&c.dst.block),
                c.dst.port
            );
            (k, c)
        })
        .collect()
}

fn diff_system(old: &System, new: &System, path: &[String], out: &mut ModelDiff) {
    let (old_by, old_keys) = keyed(old);
    let (new_by, new_keys) = keyed(new);

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

    let (oc, nc) = (connections(old, &old_keys), connections(new, &new_keys));
    let added: Vec<Connection> = nc
        .iter()
        .filter(|(k, _)| !oc.contains_key(*k))
        .map(|(_, c)| c.clone())
        .collect();
    let removed: Vec<Connection> = oc
        .iter()
        .filter(|(k, _)| !nc.contains_key(*k))
        .map(|(_, c)| c.clone())
        .collect();
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
    out.config = map_diff(&config_map(old), &config_map(new), &[]);
    out.workspace = map_diff(&old.workspace, &new.workspace, &[]);
    out.charts = diff_charts(&old.charts, &new.charts);
    out
}

/// Preserve repeated/empty IDs by occurrence instead of silently collapsing them.
fn diff_charts(old: &[crate::Chart], new: &[crate::Chart]) -> Vec<ChartDiff> {
    fn keyed(charts: &[crate::Chart]) -> BTreeMap<(&str, usize), &crate::Chart> {
        let mut counts = BTreeMap::new();
        charts
            .iter()
            .map(|chart| {
                let count = counts.entry(chart.id.as_str()).or_insert(0usize);
                *count += 1;
                ((chart.id.as_str(), *count), chart)
            })
            .collect()
    }
    let old = keyed(old);
    let new = keyed(new);
    old.keys()
        .chain(new.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|key| {
            let (chart, previous_name, change) = match (old.get(&key), new.get(&key)) {
                (Some(a), Some(b)) if a != b => (
                    *b,
                    (a.name != b.name).then(|| a.name.clone()),
                    ChartChange::Modified,
                ),
                (Some(a), None) => (*a, None, ChartChange::Removed),
                (None, Some(b)) => (*b, None, ChartChange::Added),
                _ => return None,
            };
            Some(ChartDiff {
                id: chart.id.clone(),
                name: chart.name.clone(),
                previous_name,
                change,
            })
        })
        .collect()
}

/// Raw solver settings plus the normalized fields, which some files only
/// provide through model-level properties.
fn config_map(m: &Model) -> BTreeMap<String, String> {
    let mut map = m.config.raw.clone();
    let c = &m.config;
    for (k, v) in [
        ("Solver", &c.solver),
        ("StartTime", &c.start_time),
        ("StopTime", &c.stop_time),
        ("FixedStep", &c.fixed_step),
    ] {
        if let Some(v) = v {
            map.entry(k.to_string()).or_insert_with(|| v.clone());
        }
    }
    map
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
            interface: None,
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
            charts: Vec::new(),
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

    #[test]
    fn bus_element_interface_changes_are_reported() {
        let mut old = block("1", "In", 0.0);
        old.block_type = "Inport".into();
        let interface = |port: &str, element: &str| {
            Some(PortInterface::from_properties(BTreeMap::from([
                ("PortNumber".to_string(), port.to_string()),
                ("Element".to_string(), element.to_string()),
            ])))
        };
        old.interface = interface("1", "bus.a");
        let mut new = old.clone();
        new.interface = interface("2", "bus.b");
        let m = compare_block(&old, &new);
        assert_eq!(
            m.interface,
            vec![
                ("Element".into(), Some("bus.a".into()), Some("bus.b".into())),
                ("PortNumber".into(), Some("1".into()), Some("2".into())),
            ]
        );
        assert!(!m.is_empty() && !m.layout_only());
        assert!(compare_block(&old, &old).is_empty());
    }

    #[test]
    fn structural_and_appearance_changes_are_reported() {
        let old = block("1", "a", 0.0);
        let mut new = old.clone();
        new.subsystem = Some(Box::default());
        new.ports = PortCounts::from_slice(&[2, 1]);
        new.mirrored = true;
        new.library_source = Some("simulink/Math/Gain".into());
        let m = compare_block(&old, &new);
        assert!(m.subsystem_changed && m.ports_changed && m.appearance_changed);
        assert_eq!(
            m.library_changed,
            Some((None, Some("simulink/Math/Gain".into())))
        );
        assert!(!m.layout_only());

        let mut flipped = old.clone();
        flipped.mirrored = true;
        assert!(compare_block(&old, &flipped).layout_only());
    }

    #[test]
    fn synthesized_ids_survive_parent_rename() {
        // MDL without SIDs: ids embed the parent path, which changes when
        // the parent is renamed; wiring inside must not look rewired.
        let inner = |parent: &str| {
            let mut a = block(&format!("path:m/{parent}/a"), "a", 0.0);
            let b = block(&format!("path:m/{parent}/b"), "b", 50.0);
            a.ports = PortCounts::from_slice(&[1, 1]);
            System {
                lines: vec![Line {
                    src: Some(Endpoint {
                        block: a.id.clone(),
                        port: PortRef {
                            kind: PortKind::Out,
                            index: 1,
                        },
                    }),
                    dst: Some(Endpoint {
                        block: b.id.clone(),
                        port: PortRef {
                            kind: PortKind::In,
                            index: 1,
                        },
                    }),
                    ..Default::default()
                }],
                blocks: vec![a, b],
                ..Default::default()
            }
        };
        let mut old_sub = block("path:m/P", "P", 0.0);
        old_sub.subsystem = Some(Box::new(inner("P")));
        let mut new_sub = block("path:m/Q", "Q", 0.0);
        new_sub.subsystem = Some(Box::new(inner("Q")));
        let d = diff(
            &model(System {
                blocks: vec![old_sub],
                ..Default::default()
            }),
            &model(System {
                blocks: vec![new_sub],
                ..Default::default()
            }),
        );
        assert!(d.systems.is_empty(), "{:?}", d.systems);
    }

    #[test]
    fn workspace_names_are_never_filtered() {
        let old = model(System::default());
        let mut new = old.clone();
        new.workspace.insert("ZOrder".into(), "3".into());
        let d = diff(&old, &new);
        assert_eq!(d.workspace, vec![("ZOrder".into(), None, Some("3".into()))]);
    }

    #[test]
    fn duplicate_keys_do_not_collapse() {
        let dup = |g: &str| {
            let mut b = block("path:m/x", "x", 0.0);
            b.parameters.insert("Gain".into(), g.into());
            b
        };
        let old = model(System {
            blocks: vec![dup("1"), dup("2")],
            ..Default::default()
        });
        let new = model(System {
            blocks: vec![dup("1"), dup("3")],
            ..Default::default()
        });
        let d = diff(&old, &new);
        assert_eq!(d.blocks.len(), 1);
    }
    #[test]
    fn chart_scripts_metadata_and_duplicate_ids_are_not_silently_ignored() {
        let chart: crate::Chart = serde_json::from_value(serde_json::json!({
            "id":"1", "name":"Sub/Function", "kind":"MatlabFunction",
            "states":[], "transitions":[], "junctions":[], "script":"function y=f(u); y=u; end"
        }))
        .unwrap();
        let mut a = model(System::default());
        a.charts = vec![chart.clone(), chart];
        let mut b = a.clone();
        assert!(diff(&a, &b).is_empty());
        b.charts[1].script = Some("function y=f(u); y=2*u; end".into());
        let d = diff(&a, &b);
        assert_eq!(d.charts.len(), 1);
        assert_eq!(d.charts[0].change, ChartChange::Modified);
        assert!(!d.is_empty());
        assert!(d.touches(&["Sub".into()]));
        b.charts[0].sample_time = Some("0.1".into());
        assert_eq!(diff(&a, &b).charts.len(), 2);
        b.charts.pop();
        assert!(diff(&a, &b)
            .charts
            .iter()
            .any(|c| c.change == ChartChange::Removed));
        b.charts[0].name = "New/Function".into();
        assert!(diff(&a, &b).touches(&["Sub".into()]));
        assert!(diff(&a, &b).touches(&["New".into()]));
        assert_eq!(diff(&model(System::default()), &a).charts.len(), 2);
    }
}
