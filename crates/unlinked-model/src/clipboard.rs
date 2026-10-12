//! Pasting copied blocks.
//!
//! A copy keeps the model as it was, so later edits to (or the deletion of)
//! the originals do not change what pastes; the copies get SIDs and names
//! that are free in the model being pasted into.

use crate::duplicate::copy_name;
use crate::edit::{duplicate, next_sid, system_names, Edit, EditError, SystemRef};
use crate::{BlockId, Model, Point, Rect};
use std::collections::HashSet;

/// Distance each successive paste lands further down and right.
pub const PASTE_STEP: f64 = 20.0;

/// Steps a paste elsewhere tries, to clear the blocks already there,
/// before it lands where it was copied from regardless.
const MAX_STEPS: u32 = 50;

/// The edits pasting blocks `ids` of system `system`, as they are in
/// `source`, into system `into` of `target` for the `nth` time (from 0).
/// Pasted `beside` the originals (into the system of the model they were
/// copied from), the copies land a step further down and right each time;
/// anywhere else (another system, or another model) they keep their
/// positions and, where free, their names.
pub fn paste(
    source: &Model,
    system: &SystemRef,
    ids: &[BlockId],
    nth: u32,
    target: &Model,
    into: &SystemRef,
    beside: bool,
) -> Result<Vec<Edit>, EditError> {
    let next = |m: &Model| next_sid(m).ok_or_else(|| EditError::Invalid("no SIDs left".into()));
    let first_sid = next(target)?.max(next(source)?);
    let names = system_names(target, into).ok_or_else(|| EditError::NoSystem(into.clone()))?;
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let sys = target
        .system_at(&refs)
        .ok_or_else(|| EditError::NoSystem(into.clone()))?;
    let step = if beside {
        PASTE_STEP * (f64::from(nth) + 1.0)
    } else {
        // Where they were, or else as few steps on as clears the blocks
        // already there.
        let names =
            system_names(source, system).ok_or_else(|| EditError::NoSystem(system.clone()))?;
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let from = source
            .system_at(&refs)
            .ok_or_else(|| EditError::NoSystem(system.clone()))?;
        let copied: Vec<Rect> = ids
            .iter()
            .filter_map(|id| Some(from.block(id)?.position))
            .collect();
        let overlap = |a: &Rect, b: &Rect| {
            a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
        };
        let clear = |d: f64| {
            copied.iter().all(|r| {
                let moved = Rect::new(r.left + d, r.top + d, r.right + d, r.bottom + d);
                sys.blocks.iter().all(|b| !overlap(&moved, &b.position))
            })
        };
        (nth..nth + MAX_STEPS)
            .map(|k| PASTE_STEP * f64::from(k))
            .find(|&d| clear(d))
            .unwrap_or(PASTE_STEP * f64::from(nth))
    };
    let mut edits = duplicate(source, system, ids, Point::new(step, step), first_sid)?;
    let mut taken: HashSet<String> = sys.blocks.iter().map(|b| b.name.clone()).collect();
    // The name a copy was given for its own system, or, elsewhere, its
    // original's: duplicate gives the copy of `ids[i]` SID `first_sid + i`.
    let original = |id: &BlockId| -> Option<String> {
        let i = usize::try_from(id.0.parse::<u64>().ok()?.checked_sub(first_sid)?).ok()?;
        let names = system_names(source, system)?;
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        Some(source.system_at(&refs)?.block(ids.get(i)?)?.name.clone())
    };
    for edit in &mut edits {
        // A copy only adds blocks and the lines between them, so all of it
        // moves to the other system as it is.
        match edit {
            Edit::AddBlock { system, .. }
            | Edit::SetOrientation { system, .. }
            | Edit::SetParameter { system, .. }
            | Edit::SetSignalName { system, .. }
            | Edit::Connect { system, .. } => *system = into.clone(),
            _ => {
                return Err(EditError::Invalid(
                    "a copy holds only blocks and the lines between them".into(),
                ))
            }
        }
        if let Edit::AddBlock { id, name, .. } = edit {
            if !beside {
                if let Some(original) = original(id) {
                    *name = original;
                }
            }
            if !taken.insert(name.clone()) {
                // Taken since the copy: take the next free number.
                *name = copy_name(name, |n| taken.insert(n.to_string()));
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
        let edits = paste(&copied, &vec![], &ids, 0, &now, &vec![], true).unwrap();
        apply_batch(&mut now, &edits).unwrap();
        let gain = now.root.blocks.iter().find(|b| b.name == "g1").unwrap();
        assert_eq!(gain.param("Gain"), Some("2"));
        // The internal line was copied even though the original is gone.
        let constant = now.root.blocks.iter().find(|b| b.name == "c1").unwrap();
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
            let edits = paste(&copied, &vec![], &ids, nth, &now, &vec![], true).unwrap();
            seen.extend(added(&edits));
            apply_batch(&mut now, &edits).unwrap();
        }
        assert_eq!(
            seen,
            [
                ("3".to_string(), "g1".to_string()),
                ("4".to_string(), "g2".to_string()),
                ("5".to_string(), "g3".to_string()),
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
        assert_eq!(left("g3") - left("g1"), 2.0 * PASTE_STEP);
    }

    #[test]
    fn pastes_into_another_system_where_the_blocks_were() {
        let copied = model();
        let mut now = copied.clone();
        // A subsystem holding a third block, to paste the pair into.
        apply_batch(
            &mut now,
            &[
                Edit::AddBlock {
                    system: vec![],
                    id: "3".into(),
                    block_type: "Gain".into(),
                    name: "h".into(),
                    position: Rect::new(0.0, 100.0, 30.0, 130.0),
                },
                Edit::CreateSubsystem {
                    system: vec![],
                    ids: vec!["3".into()],
                    id: "4".into(),
                    name: "Sub".into(),
                },
            ],
        )
        .unwrap();
        let into: SystemRef = vec!["4".into()];
        let edits = paste(
            &copied,
            &vec![],
            &["1".into(), "2".into()],
            0,
            &now,
            &into,
            false,
        )
        .unwrap();
        assert!(edits.iter().all(|e| e.system() == into.as_slice()));
        apply_batch(&mut now, &edits).unwrap();
        let sub = now.system_at(&["Sub"]).unwrap();
        let gain = sub.blocks.iter().find(|b| b.name == "g").unwrap();
        assert_eq!(gain.position.left, 60.0);
        assert_eq!(gain.param("Gain"), Some("2"));
        assert_eq!(sub.connections().len(), 1);
        // The originals are untouched.
        assert_eq!(now.root.blocks.iter().filter(|b| b.name == "g").count(), 1);
    }

    #[test]
    fn missing_systems_are_reported() {
        let copied = model();
        assert!(paste(
            &copied,
            &vec!["9".into()],
            &["2".into()],
            0,
            &copied,
            &vec![],
            false
        )
        .is_err());
    }

    #[test]
    fn pastes_into_another_model_where_the_blocks_were() {
        let copied = model();
        // Another model, holding a block named as one copied, and with
        // SIDs used beyond the copied model's.
        let mut other = model();
        apply_batch(
            &mut other,
            &[
                Edit::DeleteBlock {
                    system: vec![],
                    id: "2".into(),
                    disconnect: DisconnectPolicy::Disconnect,
                },
                Edit::AddBlock {
                    system: vec![],
                    id: "7".into(),
                    block_type: "Gain".into(),
                    name: "k".into(),
                    position: Rect::new(0.0, 100.0, 30.0, 130.0),
                },
            ],
        )
        .unwrap();
        let ids = ["1".into(), "2".into()];
        let edits = paste(&copied, &vec![], &ids, 0, &other, &vec![], false).unwrap();
        // Free of both models' SIDs; "g" is free there, "c" is not.
        assert_eq!(
            added(&edits),
            [
                ("8".to_string(), "c1".to_string()),
                ("9".to_string(), "g".to_string())
            ]
        );
        apply_batch(&mut other, &edits).unwrap();
        // Clear of the "c" already there: two steps on, as one still
        // overlaps it.
        let gain = other.root.blocks.iter().find(|b| b.name == "g").unwrap();
        assert_eq!(gain.position.left, 60.0 + 2.0 * PASTE_STEP);
        assert_eq!(gain.param("Gain"), Some("2"));
        assert_eq!(other.root.connections().len(), 1);
    }
}
