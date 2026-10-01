//! Keeping a subsystem block's ports in step with the port blocks inside it.
//!
//! The Inport and Outport blocks in a subsystem are the subsystem block's
//! ports, numbered by their `Port` parameter. Adding, deleting or renumbering
//! one of them changes the parent block's port count and which port each
//! connection outside lands on. [`boundary_remap`] works out that change for
//! an edit, before it is applied, so the IR and the file patchers apply the
//! same mapping.
//!
//! Only plain Inport/Outport blocks are supported: bus element ports,
//! control ports (enable, trigger, action, reset) and physical ports are
//! refused inside subsystems, as are subsystems that are masked, linked,
//! atomic, variants or Stateflow charts.

use crate::catalog;
use crate::edit::{system_names, DisconnectPolicy, Edit, EditError};
use crate::{Block, BlockId, Branch, Endpoint, Model, PortKind, System};

/// Port blocks other than plain Inport/Outport, which are not supported
/// inside subsystems yet.
const OTHER_PORT_BLOCKS: &[&str] = &[
    "EnablePort",
    "TriggerPort",
    "ActionPort",
    "ResetPort",
    "PMIOPort",
];

/// How the ports of one kind on a subsystem block change.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryRemap {
    /// The system holding the subsystem block.
    pub parent_system: Vec<BlockId>,
    /// The subsystem block.
    pub parent: BlockId,
    /// `In` for Inport blocks, `Out` for Outport blocks.
    pub kind: PortKind,
    /// For each old port (index 0 is port 1): its new number, or `None` if
    /// the port goes away.
    pub map: Vec<Option<u32>>,
    /// The new number of ports of this kind.
    pub count: u32,
    /// What to do with a connection on a port that goes away.
    pub disconnect: DisconnectPolicy,
}

fn refuse(reason: &str) -> EditError {
    EditError::SubsystemPorts(reason.into())
}

fn system_at<'a>(model: &'a Model, path: &[BlockId]) -> Option<&'a System> {
    let mut sys = &model.root;
    for id in path {
        sys = sys.block(id)?.subsystem.as_deref()?;
    }
    Some(sys)
}

fn port_kind(block_type: &str) -> Option<PortKind> {
    match block_type {
        "Inport" => Some(PortKind::In),
        "Outport" => Some(PortKind::Out),
        _ => None,
    }
}

/// The plain port blocks of `kind` in `sys`, which must be numbered 1 to n
/// without bus elements among them.
fn numbered_ports(sys: &System, block_type: &str) -> Result<u32, EditError> {
    let ports: Vec<&Block> = sys
        .blocks
        .iter()
        .filter(|b| b.block_type == block_type)
        .collect();
    if ports.iter().any(|b| b.interface.is_some()) {
        return Err(refuse("bus element ports are not supported in edits yet"));
    }
    let mut numbers: Vec<u32> = ports
        .iter()
        .map(|b| catalog::interface_port_number(b))
        .collect::<Result<_, _>>()
        .map_err(|e| refuse(&e))?;
    numbers.sort_unstable();
    let count = numbers.len() as u32;
    if !numbers.iter().copied().eq(1..=count) {
        return Err(refuse("the port blocks must be numbered 1 to n"));
    }
    Ok(count)
}

/// The block `system` is the inside of, checked to be a plain subsystem.
fn parent_block<'a>(model: &'a Model, system: &[BlockId]) -> Result<&'a Block, EditError> {
    let (id, outer) = system.split_last().expect("nested system");
    let block = system_at(model, outer)
        .and_then(|s| s.block(id))
        .ok_or_else(|| EditError::NoSystem(system.to_vec()))?;
    let flag = |name: &str| block.param(name) == Some("on");
    if block.block_type != "SubSystem"
        || block.mask.is_some()
        || block.library_source.is_some()
        || flag("TreatAsAtomicUnit")
        || flag("Variant")
        || block.stateflow_type().is_some()
    {
        return Err(refuse(
            "only plain (unmasked, unlinked, non-atomic, non-variant) subsystems can change ports",
        ));
    }
    let names = system_names(model, system).ok_or_else(|| EditError::NoSystem(system.to_vec()))?;
    if model
        .charts
        .iter()
        .any(|c| crate::stateflow::split_path(&c.name).starts_with(&names))
    {
        return Err(refuse(
            "subsystems containing Stateflow charts cannot change ports",
        ));
    }
    Ok(block)
}

/// The change `edit` makes to its subsystem's ports, or `None` if it does
/// not add, delete or renumber a port block inside a subsystem.
pub fn boundary_remap(model: &Model, edit: &Edit) -> Result<Option<BoundaryRemap>, EditError> {
    let system = edit.system();
    let Some(sys) = system_at(model, system) else {
        return Ok(None);
    };
    enum Change {
        Append,
        Remove(u32),
        Move(u32, u32),
    }
    let (block_type, change, disconnect) = match edit {
        Edit::AddBlock { block_type, .. } => {
            if OTHER_PORT_BLOCKS.contains(&block_type.as_str()) && !system.is_empty() {
                return Err(refuse(
                    "only Inport and Outport blocks can be added inside subsystems",
                ));
            }
            (block_type.clone(), Change::Append, DisconnectPolicy::Reject)
        }
        Edit::DeleteBlock { id, disconnect, .. } => {
            let Some(block) = sys.block(id) else {
                return Ok(None);
            };
            if OTHER_PORT_BLOCKS.contains(&block.block_type.as_str()) && !system.is_empty() {
                return Err(refuse(
                    "only Inport and Outport blocks can be removed from subsystems",
                ));
            }
            if port_kind(&block.block_type).is_none() || block.interface.is_some() {
                return Ok(None);
            }
            let n = catalog::interface_port_number(block).map_err(|e| refuse(&e))?;
            (block.block_type.clone(), Change::Remove(n), *disconnect)
        }
        Edit::SetParameter {
            id, name, value, ..
        } if name == "Port" => {
            let Some(block) = sys.block(id) else {
                return Ok(None);
            };
            if port_kind(&block.block_type).is_none() || block.interface.is_some() {
                return Ok(None);
            }
            let from = catalog::interface_port_number(block).map_err(|e| refuse(&e))?;
            let to = catalog::port_number(value).map_err(EditError::Invalid)?;
            (
                block.block_type.clone(),
                Change::Move(from, to),
                DisconnectPolicy::Reject,
            )
        }
        _ => return Ok(None),
    };
    let Some(kind) = port_kind(&block_type) else {
        return Ok(None);
    };
    if system.is_empty() {
        // The root's ports are the model's own; nothing outside to update.
        return Ok(None);
    }
    let parent = parent_block(model, system)?;
    let count = numbered_ports(sys, &block_type)?;
    if parent.ports.count(kind) != count {
        return Err(refuse(
            "the subsystem's ports disagree with its port blocks",
        ));
    }
    let (map, new_count): (Vec<Option<u32>>, u32) = match change {
        Change::Append => ((1..=count).map(Some).collect(), count + 1),
        Change::Remove(n) => (
            (1..=count)
                .map(|i| match i.cmp(&n) {
                    std::cmp::Ordering::Less => Some(i),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(i - 1),
                })
                .collect(),
            count - 1,
        ),
        Change::Move(from, to) => {
            if to > count {
                return Err(EditError::Invalid(format!(
                    "port number must be between 1 and {count}"
                )));
            }
            (
                (1..=count).map(|i| Some(moved(i, from, to))).collect(),
                count,
            )
        }
    };
    Ok(Some(BoundaryRemap {
        parent_system: system[..system.len() - 1].to_vec(),
        parent: system[system.len() - 1].clone(),
        kind,
        map,
        count: new_count,
        disconnect,
    }))
}

/// Where port `i` lands when port `from` moves to `to` and the ports in
/// between shift to make room, as Simulink renumbers.
pub fn moved(i: u32, from: u32, to: u32) -> u32 {
    if i == from {
        to
    } else if from < to && (from + 1..=to).contains(&i) {
        i - 1
    } else if to < from && (to..from).contains(&i) {
        i + 1
    } else {
        i
    }
}

/// Renumber the plain port blocks of `block`'s kind in `sys` for moving
/// `block` to number `to`. Returns false if `block` is not such a port.
pub(crate) fn move_port(sys: &mut System, id: &BlockId, to: u32) -> Result<bool, EditError> {
    let Some(block) = sys.block(id) else {
        return Ok(false);
    };
    let block_type = block.block_type.clone();
    if port_kind(&block_type).is_none() || block.interface.is_some() {
        return Ok(false);
    }
    let count = numbered_ports(sys, &block_type)?;
    let from = catalog::interface_port_number(block).map_err(|e| refuse(&e))?;
    if to > count {
        return Err(EditError::Invalid(format!(
            "port number must be between 1 and {count}"
        )));
    }
    for b in sys.blocks.iter_mut().filter(|b| b.block_type == block_type) {
        let n = catalog::interface_port_number(b).map_err(|e| refuse(&e))?;
        b.parameters
            .insert("Port".into(), moved(n, from, to).to_string());
    }
    Ok(true)
}

/// Whether applying `remap` would remove a connection outside the subsystem.
pub fn cuts_outside(model: &Model, remap: &BoundaryRemap) -> bool {
    let Some(sys) = system_at(model, &remap.parent_system) else {
        return false;
    };
    let cut = |ep: &Option<Endpoint>| {
        ep.as_ref().is_some_and(|ep| {
            ep.block == remap.parent
                && ep.port.kind == remap.kind
                && ep.port.index >= 1
                && remap
                    .map
                    .get(ep.port.index as usize - 1)
                    .copied()
                    .flatten()
                    .is_none()
        })
    };
    sys.lines.iter().any(|line| match remap.kind {
        PortKind::Out => cut(&line.src),
        _ => {
            let mut any = false;
            each_destination(&line.dst, &line.branches, &mut |ep| any |= cut(ep));
            any
        }
    })
}

/// Apply `remap` to the subsystem block and the lines around it.
pub(crate) fn apply_remap(model: &mut Model, remap: &BoundaryRemap) -> Result<(), EditError> {
    let mut sys = &mut model.root;
    for id in &remap.parent_system {
        sys = sys
            .blocks
            .iter_mut()
            .find(|b| &b.id == id)
            .and_then(|b| b.subsystem.as_deref_mut())
            .ok_or_else(|| EditError::NoSystem(remap.parent_system.clone()))?;
    }
    let on_parent = |ep: &Endpoint| ep.block == remap.parent && ep.port.kind == remap.kind;
    let new_index = |ep: &Endpoint| remap.map.get(ep.port.index as usize - 1).copied().flatten();
    // A connection on a port that goes away is only removed with consent.
    let mut cut = false;
    let mut visit = |ep: &Option<Endpoint>| {
        cut |= ep
            .as_ref()
            .is_some_and(|ep| on_parent(ep) && ep.port.index >= 1 && new_index(ep).is_none());
    };
    for line in &sys.lines {
        match remap.kind {
            PortKind::Out => visit(&line.src),
            _ => each_destination(&line.dst, &line.branches, &mut visit),
        }
    }
    if cut && remap.disconnect == DisconnectPolicy::Reject {
        return Err(EditError::Connected(remap.parent.clone()));
    }
    let renumber = |ep: &mut Option<Endpoint>| {
        if let Some(e) = ep.as_mut().filter(|e| on_parent(e) && e.port.index >= 1) {
            match new_index(e) {
                Some(i) => e.port.index = i,
                None => *ep = None,
            }
        }
    };
    sys.lines.retain_mut(|line| {
        match remap.kind {
            PortKind::Out => {
                renumber(&mut line.src);
                if line.src.is_none() {
                    return false;
                }
            }
            _ => {
                renumber(&mut line.dst);
                renumber_branches(&mut line.branches, &renumber);
                prune_empty(&mut line.branches);
            }
        }
        line.src.is_none() || line.dst.is_some() || !line.branches.is_empty()
    });
    let parent = sys
        .blocks
        .iter_mut()
        .find(|b| b.id == remap.parent)
        .ok_or_else(|| EditError::NoBlock(remap.parent.clone()))?;
    match remap.kind {
        PortKind::Out => parent.ports.outputs = remap.count,
        _ => parent.ports.inputs = remap.count,
    }
    Ok(())
}

fn each_destination(
    dst: &Option<Endpoint>,
    branches: &[Branch],
    f: &mut dyn FnMut(&Option<Endpoint>),
) {
    f(dst);
    for b in branches {
        each_destination(&b.dst, &b.branches, f);
    }
}

fn renumber_branches(branches: &mut [Branch], f: &dyn Fn(&mut Option<Endpoint>)) {
    for b in branches {
        f(&mut b.dst);
        renumber_branches(&mut b.branches, f);
    }
}

fn prune_empty(branches: &mut Vec<Branch>) {
    for b in branches.iter_mut() {
        prune_empty(&mut b.branches);
    }
    branches.retain(|b| b.dst.is_some() || !b.branches.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{apply_batch, Edit};
    use crate::{
        BlockStyle, Line, Mask, Orientation, PortCounts, PortRef, Rect, SimConfig, SourceFormat,
    };
    use std::collections::BTreeMap;

    fn block(id: &str, ty: &str, name: &str, port: Option<u32>, ports: [u32; 2]) -> Block {
        Block {
            id: id.into(),
            block_type: ty.into(),
            name: name.into(),
            position: Rect::new(0.0, 0.0, 30.0, 30.0),
            orientation: Orientation::Right,
            mirrored: false,
            ports: PortCounts::from_slice(&ports),
            parameters: port
                .map(|n| BTreeMap::from([("Port".to_string(), n.to_string())]))
                .unwrap_or_default(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: BlockStyle::default(),
            interface: None,
        }
    }

    fn ep(id: &str, kind: PortKind, index: u32) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef { kind, index },
        }
    }

    fn wire(src: Endpoint, dst: Endpoint) -> Line {
        Line {
            src: Some(src),
            dst: Some(dst),
            ..Default::default()
        }
    }

    /// Root: constants a and b feed subsystem s's inputs 1 and 2; s's output
    /// feeds terminator t. Inside s: In1, In2, Out1.
    fn model() -> Model {
        let mut s = block("10", "SubSystem", "s", None, [2, 1]);
        s.subsystem = Some(Box::new(System {
            blocks: vec![
                block("11", "Inport", "In1", Some(1), [0, 1]),
                block("12", "Inport", "In2", Some(2), [0, 1]),
                block("13", "Outport", "Out1", Some(1), [1, 0]),
            ],
            ..Default::default()
        }));
        Model {
            name: "m".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: SimConfig::default(),
            root: System {
                blocks: vec![
                    block("1", "Constant", "a", None, [0, 1]),
                    block("2", "Constant", "b", None, [0, 1]),
                    s,
                    block("3", "Terminator", "t", None, [1, 0]),
                ],
                lines: vec![
                    wire(ep("1", PortKind::Out, 1), ep("10", PortKind::In, 1)),
                    wire(ep("2", PortKind::Out, 1), ep("10", PortKind::In, 2)),
                    wire(ep("10", PortKind::Out, 1), ep("3", PortKind::In, 1)),
                ],
                ..Default::default()
            },
            workspace: BTreeMap::new(),
            charts: Vec::new(),
        }
    }

    fn inner<'a>(m: &'a Model, id: &str) -> &'a Block {
        m.root.blocks[2]
            .subsystem
            .as_ref()
            .unwrap()
            .block(&id.into())
            .unwrap()
    }

    fn delete(id: &str, disconnect: DisconnectPolicy) -> Edit {
        Edit::DeleteBlock {
            system: vec!["10".into()],
            id: id.into(),
            disconnect,
        }
    }

    #[test]
    fn deleting_a_port_block_removes_its_outer_connection_only_with_consent() {
        let mut m = model();
        assert_eq!(
            apply_batch(&mut m, &[delete("11", DisconnectPolicy::Reject)])
                .unwrap_err()
                .error,
            EditError::Connected("10".into())
        );
        apply_batch(&mut m, &[delete("11", DisconnectPolicy::Disconnect)]).unwrap();
        assert_eq!(m.root.blocks[2].ports.inputs, 1);
        assert_eq!(inner(&m, "12").param("Port"), Some("1"));
        // a's line is gone; b's now lands on input 1.
        assert_eq!(m.root.lines.len(), 2);
        assert_eq!(m.root.lines[0].dst, Some(ep("10", PortKind::In, 1)));
        assert_eq!(m.root.lines[0].src, Some(ep("2", PortKind::Out, 1)));
        // Deleting the output drops the line it drove.
        apply_batch(&mut m, &[delete("13", DisconnectPolicy::Disconnect)]).unwrap();
        assert_eq!(m.root.blocks[2].ports.outputs, 0);
        assert_eq!(m.root.lines.len(), 1);
    }

    #[test]
    fn renumbering_a_port_moves_the_outer_connections_with_it() {
        let mut m = model();
        let renumber = Edit::SetParameter {
            system: vec!["10".into()],
            id: "12".into(),
            name: "Port".into(),
            value: "1".into(),
        };
        apply_batch(&mut m, &[renumber]).unwrap();
        assert_eq!(inner(&m, "12").param("Port"), Some("1"));
        assert_eq!(inner(&m, "11").param("Port"), Some("2"));
        // The signals keep their meaning: a still reaches In1, now port 2.
        assert_eq!(m.root.lines[0].dst, Some(ep("10", PortKind::In, 2)));
        assert_eq!(m.root.lines[1].dst, Some(ep("10", PortKind::In, 1)));
        assert!(matches!(
            Edit::SetParameter {
                system: vec!["10".into()],
                id: "12".into(),
                name: "Port".into(),
                value: "3".into(),
            }
            .apply(&mut m),
            Err(EditError::Invalid(_))
        ));
    }

    #[test]
    fn cuts_outside_reports_a_wired_removed_port() {
        let mut m = model();
        let remap = |m: &Model| {
            boundary_remap(m, &delete("11", DisconnectPolicy::Disconnect))
                .unwrap()
                .unwrap()
        };
        assert!(cuts_outside(&m, &remap(&m)));
        m.root.lines.remove(0);
        assert!(!cuts_outside(&m, &remap(&m)));
    }

    #[test]
    fn moved_shifts_the_ports_between() {
        let order = |from, to| (1..=4).map(|i| moved(i, from, to)).collect::<Vec<_>>();
        assert_eq!(order(4, 1), [2, 3, 4, 1]);
        assert_eq!(order(1, 3), [3, 1, 2, 4]);
        assert_eq!(order(2, 2), [1, 2, 3, 4]);
    }

    #[test]
    fn adding_a_port_block_grows_the_subsystem() {
        let mut m = model();
        let sid = crate::edit::next_sid(&m).unwrap().to_string();
        apply_batch(
            &mut m,
            &[Edit::AddBlock {
                system: vec!["10".into()],
                id: sid.as_str().into(),
                block_type: "Inport".into(),
                name: "In3".into(),
                position: Rect::new(0.0, 100.0, 30.0, 115.0),
            }],
        )
        .unwrap();
        assert_eq!(m.root.blocks[2].ports.inputs, 3);
        assert_eq!(inner(&m, &sid).param("Port"), Some("3"));
        assert_eq!(m.root.lines.len(), 3);
    }

    #[test]
    fn unsupported_subsystems_and_port_kinds_are_refused() {
        let enable = Edit::AddBlock {
            system: vec!["10".into()],
            id: "99".into(),
            block_type: "EnablePort".into(),
            name: "en".into(),
            position: Rect::new(0.0, 0.0, 30.0, 30.0),
        };
        assert!(matches!(
            boundary_remap(&model(), &enable),
            Err(EditError::SubsystemPorts(_))
        ));
        let mut masked = model();
        masked.root.blocks[2].mask = Some(Mask::default());
        assert!(matches!(
            boundary_remap(&masked, &delete("11", DisconnectPolicy::Disconnect)),
            Err(EditError::SubsystemPorts(_))
        ));
        let mut atomic = model();
        atomic.root.blocks[2]
            .parameters
            .insert("TreatAsAtomicUnit".into(), "on".into());
        assert!(boundary_remap(&atomic, &delete("11", DisconnectPolicy::Disconnect)).is_err());
        // The root's port blocks have nothing outside to update.
        let mut root = model();
        root.root
            .blocks
            .push(block("20", "Inport", "r1", Some(1), [0, 1]));
        assert_eq!(
            boundary_remap(
                &root,
                &Edit::DeleteBlock {
                    system: vec![],
                    id: "20".into(),
                    disconnect: DisconnectPolicy::Disconnect
                }
            ),
            Ok(None)
        );
    }
}
