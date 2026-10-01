//! Edits a user makes to a diagram.
//!
//! The same [`Edit`] values are applied to the in-memory IR (for an
//! immediate preview) and, by `unlinked-import`'s patcher, to the original
//! model file, so that everything the IR does not model survives a save.
//!
//! Edits address systems and blocks by [`BlockId`] as imported from the
//! pinned base version, never by name: names change when blocks are
//! renamed, including earlier in the same batch.

use crate::catalog::{self, PortResolution};
use crate::validation::{validate_structure, DiagnosticTarget, Severity};
use crate::{
    Block, BlockId, BlockStyle, Branch, Chart, Endpoint, Line, Model, Orientation, PortKind, Rect,
    System,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// Add a palette block with its catalog creation parameters. `id` is
    /// its SID, at least [`next_sid`] of the model at that point.
    AddBlock {
        system: SystemRef,
        id: BlockId,
        block_type: String,
        name: String,
        position: Rect,
    },
    /// Connect an output to an undriven input. A source that already has a
    /// line gains a branch.
    Connect {
        system: SystemRef,
        src: Endpoint,
        dst: Endpoint,
    },
    /// Remove the connection into the input `dst`, along with branches and
    /// lines left leading nowhere.
    Disconnect { system: SystemRef, dst: Endpoint },
}

/// An edit that could not be applied, and its position in the batch.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("edit {index}: {error}")]
pub struct BatchError {
    pub index: usize,
    pub error: EditError,
}

/// Apply `edits` in order, all or nothing: on failure `model` is unchanged.
/// A batch that adds a structural error (see [`structural_regression`]) is
/// refused.
pub fn apply_batch(model: &mut Model, edits: &[Edit]) -> Result<(), BatchError> {
    let mut next = model.clone();
    for (index, edit) in edits.iter().enumerate() {
        edit.apply(&mut next)
            .map_err(|error| BatchError { index, error })?;
    }
    if let Err(error) = structural_regression(model, &next) {
        // Attribute the regression to the first edit that introduces it.
        let mut partial = model.clone();
        let index = edits
            .iter()
            .position(|edit| {
                let _ = edit.apply(&mut partial);
                structural_regression(model, &partial).is_err()
            })
            .unwrap_or(edits.len().saturating_sub(1));
        return Err(BatchError { index, error });
    }
    *model = next;
    Ok(())
}

/// Refuse `after` if it has more structural errors of some kind than
/// `before`. Errors already present in an imported model do not block
/// unrelated edits.
pub fn structural_regression(before: &Model, after: &Model) -> Result<(), EditError> {
    // Errors are identified by code and target. Line roots are renumbered
    // by unrelated line edits, so lines are identified by their endpoint.
    let errors = |model: &Model| {
        let report = validate_structure(model);
        if report.truncated {
            return Err(EditError::Structure(
                "the model is too large or has too many problems to check edits".into(),
            ));
        }
        let mut errors: BTreeMap<(String, String), (usize, String)> = BTreeMap::new();
        for d in report.diagnostics {
            if d.severity != Severity::Error {
                continue;
            }
            let target = match d.target {
                DiagnosticTarget::Line {
                    system, endpoint, ..
                } => format!("line {system:?} {endpoint:?}"),
                other => format!("{other:?}"),
            };
            errors.entry((d.code, target)).or_insert((0, d.message)).0 += 1;
        }
        Ok(errors)
    };
    let before = errors(before)?;
    for (key, (n, message)) in errors(after)? {
        if before.get(&key).map_or(0, |b| b.0) < n {
            return Err(EditError::Structure(format!("{}: {message}", key.0)));
        }
    }
    Ok(())
}

/// The lowest SID a new block may take: above every numeric block SID and
/// the file's `SIDHighWatermark`, so SIDs of deleted blocks are not reused.
/// `None` once SIDs are exhausted.
pub fn next_sid(model: &Model) -> Option<u64> {
    fn highest(sys: &System) -> u64 {
        sys.blocks
            .iter()
            .map(|b| {
                let own = b.id.0.parse().unwrap_or(0);
                own.max(b.subsystem.as_deref().map_or(0, highest))
            })
            .max()
            .unwrap_or(0)
    }
    let watermark = model
        .root
        .properties
        .get(SID_WATERMARK)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    highest(&model.root).max(watermark).checked_add(1)
}

/// Root system property recording the highest SID ever allocated.
pub const SID_WATERMARK: &str = "SIDHighWatermark";

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
    #[error("{0} is not a port of that block")]
    NoPort(Endpoint),
    #[error("{0} is already driven")]
    Driven(Endpoint),
    #[error("{0} is not connected")]
    NotConnected(Endpoint),
    #[error("the edit would add a structural error ({0})")]
    Structure(String),
    #[error("adding or removing a subsystem's port blocks is not supported yet")]
    SubsystemPorts,
}

/// Blocks that define a port of the subsystem containing them.
const PORT_BLOCKS: &[&str] = &[
    "Inport",
    "Outport",
    "EnablePort",
    "TriggerPort",
    "ActionPort",
    "ResetPort",
    "PMIOPort",
];

/// As Simulink does, renumber the ports above a deleted Inport/Outport so
/// the numbers stay contiguous. Plain and bus element ports share the
/// numbering; a bus number still used by another element is no gap.
fn close_port_gap(sys: &mut System, removed: &Block) {
    let kind = removed.block_type.as_str();
    if !matches!(kind, "Inport" | "Outport") {
        return;
    }
    let number = |b: &Block| catalog::interface_port_number(b).ok();
    let Some(gap) = number(removed) else {
        return;
    };
    if sys
        .blocks
        .iter()
        .any(|b| b.block_type == kind && number(b) == Some(gap))
    {
        return;
    }
    for b in sys.blocks.iter_mut().filter(|b| b.block_type == kind) {
        let Some(n) = number(b).filter(|&n| n > gap) else {
            continue;
        };
        let renumbered = (n - 1).to_string();
        match &mut b.interface {
            Some(interface) => {
                interface.port_number = Some(n - 1);
                interface
                    .raw
                    .insert("PortNumber".into(), renumbered.clone());
                if let Some(port) = b.parameters.get_mut("Port") {
                    *port = renumbered;
                }
            }
            None => {
                b.parameters.insert("Port".into(), renumbered);
            }
        }
    }
}

impl Edit {
    pub fn system(&self) -> &[BlockId] {
        match self {
            Edit::MoveBlock { system, .. }
            | Edit::SetParameter { system, .. }
            | Edit::RenameBlock { system, .. }
            | Edit::DeleteBlock { system, .. }
            | Edit::AddBlock { system, .. }
            | Edit::Connect { system, .. }
            | Edit::Disconnect { system, .. } => system,
        }
    }

    /// The existing block the edit changes, if it targets one.
    pub fn block(&self) -> Option<&BlockId> {
        match self {
            Edit::MoveBlock { id, .. }
            | Edit::SetParameter { id, .. }
            | Edit::RenameBlock { id, .. }
            | Edit::DeleteBlock { id, .. } => Some(id),
            Edit::AddBlock { .. } | Edit::Connect { .. } | Edit::Disconnect { .. } => None,
        }
    }

    /// Check an edit's values before applying it anywhere.
    pub fn validate(&self) -> Result<(), EditError> {
        match self {
            Edit::MoveBlock { position, .. } => check_rect(position)?,
            Edit::SetParameter { name, value, .. } => {
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(EditError::Invalid(format!("bad parameter name {name:?}")));
                }
                if value.len() > 64 * 1024 {
                    return Err(EditError::Invalid("parameter value too long".into()));
                }
            }
            Edit::RenameBlock { name, .. } => check_name(name)?,
            Edit::AddBlock { name, position, .. } => {
                check_rect(position)?;
                check_name(name)?;
            }
            Edit::DeleteBlock { .. } | Edit::Connect { .. } | Edit::Disconnect { .. } => {}
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
        let id = match self {
            Edit::AddBlock {
                system,
                id,
                block_type,
                name,
                position,
            } => return add_block(model, system, id, block_type, name, *position),
            Edit::Connect { system, src, dst } => {
                return connect(system_mut(model, system)?, src, dst)
            }
            Edit::Disconnect { system, dst } => return disconnect(system_mut(model, system)?, dst),
            Edit::MoveBlock { id, .. }
            | Edit::SetParameter { id, .. }
            | Edit::RenameBlock { id, .. }
            | Edit::DeleteBlock { id, .. } => id,
        };
        let names = system_names(model, self.system())
            .ok_or_else(|| EditError::NoSystem(self.system().to_vec()))?;
        let sys = system_mut(model, self.system())?;
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
                let block = &mut sys.blocks[index];
                if let Some(descriptor) = native_descriptor(block) {
                    let edit = descriptor
                        .check_edit(&block.parameters, name, value)
                        .map_err(EditError::Invalid)?;
                    // Removed ports that are still connected are caught by
                    // the batch's structural check.
                    if let (None | Some(true), PortResolution::Known(ports)) =
                        (edit.changes_ports, edit.ports)
                    {
                        block.ports = ports;
                    }
                }
                set_parameter(block, name, value)
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
                if !self.system().is_empty()
                    && PORT_BLOCKS.contains(&sys.blocks[index].block_type.as_str())
                {
                    return Err(EditError::SubsystemPorts);
                }
                let removed = sys.blocks.remove(index);
                close_port_gap(sys, &removed);
                // Lines not attached to the block are left alone, even if
                // they were already dangling.
                sys.lines.retain_mut(|l| {
                    if !touches(l, id) {
                        return true;
                    }
                    if ends_at(&l.src, id) {
                        return false;
                    }
                    prune(&mut l.dst, &mut l.branches, &|e| &e.block == id);
                    l.dst.is_some() || !l.branches.is_empty()
                });
            }
            Edit::AddBlock { .. } | Edit::Connect { .. } | Edit::Disconnect { .. } => {
                unreachable!("applied above")
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

/// Drop destinations matching `hit` and branches left with nowhere to go.
fn prune(dst: &mut Option<Endpoint>, branches: &mut Vec<Branch>, hit: &dyn Fn(&Endpoint) -> bool) {
    if dst.as_ref().is_some_and(hit) {
        *dst = None;
    }
    for b in branches.iter_mut() {
        prune(&mut b.dst, &mut b.branches, hit);
    }
    branches.retain(|b| b.dst.is_some() || !b.branches.is_empty());
}

/// Whether `dst` is a destination anywhere in `line`.
fn drives(line: &Line, dst: &Endpoint) -> bool {
    fn branches(bs: &[Branch], dst: &Endpoint) -> bool {
        bs.iter()
            .any(|b| b.dst.as_ref() == Some(dst) || branches(&b.branches, dst))
    }
    line.dst.as_ref() == Some(dst) || branches(&line.branches, dst)
}

fn check_rect(p: &Rect) -> Result<(), EditError> {
    let finite = [p.left, p.top, p.right, p.bottom]
        .iter()
        .all(|v| v.is_finite());
    if !finite || p.right <= p.left || p.bottom <= p.top {
        return Err(EditError::Invalid(
            "block position must be a positive-size rectangle".into(),
        ));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), EditError> {
    if name.trim().is_empty() || name.len() > 1024 {
        return Err(EditError::Invalid(
            "block names must be 1..=1024 bytes and not blank".into(),
        ));
    }
    Ok(())
}

/// The catalog entry for a plain native block, whose ports follow from its
/// parameters. Masked, linked and subsystem blocks declare their own.
fn native_descriptor(block: &Block) -> Option<&'static catalog::BlockDescriptor> {
    let native =
        block.mask.is_none() && block.library_source.is_none() && block.subsystem.is_none();
    catalog::find(&block.block_type).filter(|_| native)
}

fn add_block(
    model: &mut Model,
    system: &[BlockId],
    id: &BlockId,
    block_type: &str,
    name: &str,
    position: Rect,
) -> Result<(), EditError> {
    let descriptor = catalog::find(block_type)
        .filter(|d| d.creatable && d.source_block.is_none())
        .ok_or_else(|| EditError::Invalid(format!("{block_type:?} is not in the block palette")))?;
    if !system.is_empty() && PORT_BLOCKS.contains(&descriptor.type_key) {
        return Err(EditError::SubsystemPorts);
    }
    let next = next_sid(model).ok_or_else(|| EditError::Invalid("no SIDs left".into()))?;
    let sid =
        id.0.parse::<u64>()
            .ok()
            .filter(|&n| n >= next && id.0 == n.to_string())
            .ok_or_else(|| {
                EditError::Invalid(format!(
                    "a new block's id must be an unused SID of at least {next}"
                ))
            })?;
    let sys = system_mut(model, system)?;
    if sys.blocks.iter().any(|b| b.name == name) {
        return Err(EditError::DuplicateName(name.into()));
    }
    let parameters = descriptor
        .creation_parameters_in(sys)
        .map_err(EditError::Invalid)?;
    let PortResolution::Known(ports) = descriptor.resolve_ports(&parameters) else {
        return Err(EditError::Invalid(format!(
            "the ports of a new {block_type} cannot be resolved"
        )));
    };
    sys.blocks.push(Block {
        id: id.clone(),
        block_type: descriptor.type_key.into(),
        name: name.into(),
        position,
        orientation: Orientation::Right,
        mirrored: false,
        ports,
        parameters,
        mask: None,
        library_source: None,
        subsystem: None,
        style: BlockStyle::default(),
        interface: None,
    });
    // Recorded even where the file had none, so deleting the block cannot
    // free its SID for reuse.
    model
        .root
        .properties
        .insert(SID_WATERMARK.into(), sid.to_string());
    Ok(())
}

/// Check that `ep` is an existing signal port of the right direction.
fn check_port(sys: &System, ep: &Endpoint, source: bool) -> Result<(), EditError> {
    let block = sys
        .block(&ep.block)
        .ok_or_else(|| EditError::NoBlock(ep.block.clone()))?;
    let direction = if source {
        matches!(ep.port.kind, PortKind::Out | PortKind::State)
    } else {
        matches!(
            ep.port.kind,
            PortKind::In
                | PortKind::Enable
                | PortKind::Trigger
                | PortKind::IfAction
                | PortKind::Reset
        )
    };
    // A native block whose count depends on an expression may have stale
    // declared ports; rewiring waits until the count is a literal.
    let ports = match native_descriptor(block).map(|d| d.resolve_ports(&block.parameters)) {
        Some(PortResolution::Known(ports)) => ports,
        Some(
            PortResolution::Unresolved { parameter } | PortResolution::Invalid { parameter, .. },
        ) => {
            return Err(EditError::Invalid(format!(
                "the ports of {:?} depend on {parameter}, which must be a literal to connect",
                block.name
            )))
        }
        None => block.ports,
    };
    if !direction || ep.port.index == 0 || ep.port.index > ports.count(ep.port.kind) {
        return Err(EditError::NoPort(ep.clone()));
    }
    Ok(())
}

fn connect(sys: &mut System, src: &Endpoint, dst: &Endpoint) -> Result<(), EditError> {
    check_port(sys, src, true)?;
    check_port(sys, dst, false)?;
    if sys.lines.iter().any(|l| drives(l, dst)) {
        return Err(EditError::Driven(dst.clone()));
    }
    let new = Branch {
        dst: Some(dst.clone()),
        ..Default::default()
    };
    match sys.lines.iter_mut().find(|l| l.src.as_ref() == Some(src)) {
        // The existing destination becomes the first branch; the line's
        // own vertices stay as the trunk.
        Some(line) => {
            if let Some(old) = line.dst.take() {
                line.branches.insert(
                    0,
                    Branch {
                        dst: Some(old),
                        ..Default::default()
                    },
                );
            }
            line.branches.push(new);
        }
        None => sys.lines.push(Line {
            src: Some(src.clone()),
            dst: Some(dst.clone()),
            ..Default::default()
        }),
    }
    Ok(())
}

fn disconnect(sys: &mut System, dst: &Endpoint) -> Result<(), EditError> {
    if !sys.lines.iter().any(|l| drives(l, dst)) {
        return Err(EditError::NotConnected(dst.clone()));
    }
    sys.lines.retain_mut(|l| {
        if !drives(l, dst) {
            return true;
        }
        prune(&mut l.dst, &mut l.branches, &|e| e == dst);
        l.dst.is_some() || !l.branches.is_empty()
    });
    Ok(())
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

    fn add(id: &str, block_type: &str, name: &str) -> Edit {
        Edit::AddBlock {
            system: vec![],
            id: id.into(),
            block_type: block_type.into(),
            name: name.into(),
            position: Rect::new(0.0, 100.0, 30.0, 130.0),
        }
    }

    fn connect(src: &str, dst: &str, index: u32) -> Edit {
        Edit::Connect {
            system: vec![],
            src: ep(src, PortKind::Out),
            dst: Endpoint {
                block: dst.into(),
                port: PortRef {
                    kind: PortKind::In,
                    index,
                },
            },
        }
    }

    fn disconnect(dst: &str) -> Edit {
        Edit::Disconnect {
            system: vec![],
            dst: ep(dst, PortKind::In),
        }
    }

    #[test]
    fn connect_branches_existing_lines_and_disconnect_prunes() {
        let mut m = model();
        assert_eq!(next_sid(&m), Some(4));
        add("4", "Gain", "d").apply(&mut m).unwrap();
        connect("1", "4", 1).apply(&mut m).unwrap();
        assert_eq!(m.root.lines.len(), 1);
        assert_eq!(m.root.lines[0].branches.len(), 3);
        assert_eq!(
            connect("2", "4", 1).apply(&mut m),
            Err(EditError::Driven(ep("4", PortKind::In)))
        );
        disconnect("4").apply(&mut m).unwrap();
        assert_eq!(m.root.lines[0].branches.len(), 2);
        assert_eq!(
            disconnect("4").apply(&mut m),
            Err(EditError::NotConnected(ep("4", PortKind::In)))
        );
        disconnect("2").apply(&mut m).unwrap();
        disconnect("3").apply(&mut m).unwrap();
        assert!(m.root.lines.is_empty());

        // A single-destination line keeps its vertices as the trunk.
        connect("2", "3", 1).apply(&mut m).unwrap();
        m.root.lines[0].points = vec![Point::new(50.0, 15.0)];
        connect("2", "4", 1).apply(&mut m).unwrap();
        let line = &m.root.lines[0];
        assert_eq!((line.dst.as_ref(), line.points.len()), (None, 1));
        let dsts: Vec<_> = line.branches.iter().map(|b| b.dst.clone()).collect();
        assert_eq!(
            dsts,
            [Some(ep("3", PortKind::In)), Some(ep("4", PortKind::In))]
        );
    }

    #[test]
    fn connections_need_existing_ports_of_the_right_direction() {
        let mut m = model();
        assert_eq!(
            connect("2", "3", 2).apply(&mut m),
            Err(EditError::NoPort(Endpoint {
                block: "3".into(),
                port: PortRef {
                    kind: PortKind::In,
                    index: 2
                }
            }))
        );
        let backwards = Edit::Connect {
            system: vec![],
            src: ep("2", PortKind::In),
            dst: ep("3", PortKind::In),
        };
        assert!(matches!(backwards.apply(&mut m), Err(EditError::NoPort(_))));
        assert!(matches!(
            connect("2", "missing", 1).apply(&mut m),
            Err(EditError::NoBlock(_))
        ));
    }

    #[test]
    fn added_blocks_take_fresh_sids_and_catalog_parameters() {
        let mut m = model();
        m.root.properties.insert(SID_WATERMARK.into(), "40".into());
        assert_eq!(next_sid(&m), Some(41));
        for id in ["4", "041", "x"] {
            assert!(matches!(
                add(id, "Gain", "d").apply(&mut m),
                Err(EditError::Invalid(_))
            ));
        }
        assert!(matches!(
            add("41", "NoSuchBlock", "d").apply(&mut m),
            Err(EditError::Invalid(_))
        ));
        assert_eq!(
            add("41", "Gain", "a").apply(&mut m),
            Err(EditError::DuplicateName("a".into()))
        );
        add("41", "Sum", "s").apply(&mut m).unwrap();
        assert_eq!(m.root.properties[SID_WATERMARK], "41");
        let sum = m.root.block(&"41".into()).unwrap();
        assert_eq!(sum.param("Inputs"), Some("++"));
        assert_eq!(sum.ports, PortCounts::from_slice(&[2, 1]));

        add("42", "Inport", "in1").apply(&mut m).unwrap();
        add("43", "Inport", "in2").apply(&mut m).unwrap();
        let port = |id: &str| m.root.block(&id.into()).unwrap().param("Port");
        assert_eq!((port("42"), port("43")), (Some("1"), Some("2")));
    }

    #[test]
    fn parameter_edits_resize_ports_but_cannot_strand_lines() {
        let mut m = model();
        let inputs = |value: &str| Edit::SetParameter {
            system: vec![],
            id: "4".into(),
            name: "Inputs".into(),
            value: value.into(),
        };
        apply_batch(
            &mut m,
            &[add("4", "Sum", "s"), inputs("+-+"), connect("2", "4", 3)],
        )
        .unwrap();
        assert_eq!(m.root.block(&"4".into()).unwrap().ports.inputs, 3);
        let before = m.clone();
        let err = apply_batch(&mut m, &[inputs("+-+"), inputs("+-")]).unwrap_err();
        assert_eq!(err.index, 1);
        assert!(matches!(err.error, EditError::Structure(_)));
        assert_eq!(m, before);
        assert!(matches!(
            inputs("**").apply(&mut m),
            Err(EditError::Invalid(_))
        ));
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
