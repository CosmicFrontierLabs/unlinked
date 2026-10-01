//! Pasting copied blocks.
//!
//! A copy keeps the model as it was, so later edits to (or the deletion of)
//! the originals do not change what pastes; the copies get SIDs and names
//! that are free in the model being pasted into.

use crate::edit::{duplicate, next_sid, system_names, Edit, EditError, SystemRef};
use crate::{BlockId, Model, Point};
use std::collections::HashSet;

/// Distance each successive paste lands further down and right.
pub const PASTE_STEP: f64 = 20.0;

/// The edits pasting blocks `ids` of system `system`, as they are in
/// `source`, into the same system of `target` for the `nth` time (from 0).
pub fn paste(
    source: &Model,
    system: &SystemRef,
    ids: &[BlockId],
    nth: u32,
    target: &Model,
) -> Result<Vec<Edit>, EditError> {
    let next = |m: &Model| next_sid(m).ok_or_else(|| EditError::Invalid("no SIDs left".into()));
    let first_sid = next(target)?.max(next(source)?);
    let step = PASTE_STEP * (f64::from(nth) + 1.0);
    let mut edits = duplicate(source, system, ids, Point::new(step, step), first_sid)?;
    let names = system_names(target, system).ok_or_else(|| EditError::NoSystem(system.clone()))?;
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let sys = target
        .system_at(&refs)
        .ok_or_else(|| EditError::NoSystem(system.clone()))?;
    let mut taken: HashSet<String> = sys.blocks.iter().map(|b| b.name.clone()).collect();
    for edit in &mut edits {
        if let Edit::AddBlock { name, .. } = edit {
            if !taken.insert(name.clone()) {
                // Taken since the copy: take the next free "… copyN".
                let base = match name.rsplit_once(" copy") {
                    Some((base, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                        base.to_string()
                    }
                    _ => name.clone(),
                };
                *name = (1..)
                    .map(|i| format!("{base} copy{i}"))
                    .find(|n| taken.insert(n.clone()))
                    .expect("some suffix is free");
            }
        }
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{apply_batch, DisconnectPolicy};
    use crate::{Endpoint, PortKind, PortRef, Rect, SimConfig, SourceFormat, System};
    use std::collections::BTreeMap;

    fn ep(id: &str, kind: PortKind) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef { kind, index: 1 },
        }
    }

    /// A constant feeding a gain of 2.
    fn model() -> Model {
        let mut m = Model {
            name: "m".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: SimConfig::default(),
            root: System::default(),
            workspace: BTreeMap::new(),
            type_defaults: Default::default(),
            charts: vec![],
        };
        let add = |id: &str, ty: &str, name: &str, x: f64| Edit::AddBlock {
            system: vec![],
            id: id.into(),
            block_type: ty.into(),
            name: name.into(),
            position: Rect::new(x, 0.0, x + 30.0, 30.0),
        };
        let set_gain = Edit::SetParameter {
            system: vec![],
            id: "2".into(),
            name: "Gain".into(),
            value: "2".into(),
        };
        let wire = Edit::Connect {
            system: vec![],
            src: ep("1", PortKind::Out),
            dst: ep("2", PortKind::In),
        };
        apply_batch(
            &mut m,
            &[
                add("1", "Constant", "c", 0.0),
                add("2", "Gain", "g", 60.0),
                set_gain,
                wire,
            ],
        )
        .unwrap();
        m
    }

    fn added(edits: &[Edit]) -> Vec<(String, String)> {
        edits
            .iter()
            .filter_map(|e| match e {
                Edit::AddBlock { id, name, .. } => Some((id.0.clone(), name.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pastes_the_copy_time_blocks_after_edits_and_deletion() {
        let copied = model();
        let ids = ["1".into(), "2".into()];
        let mut now = copied.clone();
        apply_batch(
            &mut now,
            &[
                Edit::SetParameter {
                    system: vec![],
                    id: "2".into(),
                    name: "Gain".into(),
                    value: "9".into(),
                },
                Edit::Disconnect {
                    system: vec![],
                    dst: ep("2", PortKind::In),
                },
                Edit::DeleteBlock {
                    system: vec![],
                    id: "2".into(),
                    disconnect: DisconnectPolicy::Disconnect,
                },
            ],
        )
        .unwrap();
        let edits = paste(&copied, &vec![], &ids, 0, &now).unwrap();
        apply_batch(&mut now, &edits).unwrap();
        let gain = now
            .root
            .blocks
            .iter()
            .find(|b| b.name == "g copy1")
            .unwrap();
        assert_eq!(gain.param("Gain"), Some("2"));
        // The internal line was copied even though the original is gone.
        let constant = now
            .root
            .blocks
            .iter()
            .find(|b| b.name == "c copy1")
            .unwrap();
        assert!(now
            .root
            .connections()
            .iter()
            .any(|c| c.src.block == constant.id && c.dst.block == gain.id));
    }

    #[test]
    fn repeated_pastes_take_fresh_sids_and_names() {
        let copied = model();
        let ids = ["2".into()];
        let mut now = copied.clone();
        let mut seen = Vec::new();
        for nth in 0..3 {
            let edits = paste(&copied, &vec![], &ids, nth, &now).unwrap();
            seen.extend(added(&edits));
            apply_batch(&mut now, &edits).unwrap();
        }
        assert_eq!(
            seen,
            [
                ("3".to_string(), "g copy1".to_string()),
                ("4".to_string(), "g copy2".to_string()),
                ("5".to_string(), "g copy3".to_string()),
            ]
        );
        // Each paste lands one step further.
        let left = |name: &str| {
            now.root
                .blocks
                .iter()
                .find(|b| b.name == name)
                .unwrap()
                .position
                .left
        };
        assert_eq!(left("g copy3") - left("g copy1"), 2.0 * PASTE_STEP);
    }

    #[test]
    fn missing_systems_are_reported() {
        let copied = model();
        assert!(paste(&copied, &vec!["9".into()], &["2".into()], 0, &copied).is_err());
    }
}
