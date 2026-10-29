//! Commented blocks are excluded from simulation.
//!
//! Simulink semantics ("Comment Out or Through Blocks",
//! https://www.mathworks.com/help/simulink/ug/comment-out-blocks.html):
//! `Commented=on` (Comment Out) removes the block; the signals into it are
//! terminated and every destination its outputs fed is grounded, so it reads
//! 0. `Commented=through` (Comment Through) removes the block and passes input
//! *i* straight to output *i*; Simulink only offers it for blocks with the
//! same number of input and output ports and no control or connection ports,
//! so other blocks are refused here too. Inport, Outport, Goto Tag Visibility
//! and Data Store Memory blocks cannot be commented at all.
//!
//! This pass runs after Goto/From routing and chart lowering, so a commented
//! Goto grounds its From blocks and a commented MATLAB Function block is
//! dropped as one node. Removed blocks never have their parameters evaluated.
use crate::{block_error, Error};
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::{
    catalog, Block, BlockId, Commented, Endpoint, Line, Model, PortCounts, PortKind, PortRef,
    System,
};

/// Why this block's `Commented` setting cannot be honoured, if it cannot.
/// Unknown values, blocks Simulink never comments, and comment-through on
/// blocks whose ports do not pair up are all refused explicitly.
pub fn refusal(block: &Block) -> Option<String> {
    let setting = match block.commented() {
        Ok(Commented::Off) => return None,
        Ok(setting) => setting,
        Err(value) => {
            return Some(format!(
                "unsupported Commented={value}; expected off, on or through"
            ))
        }
    };
    if matches!(
        block.block_type.as_str(),
        "Inport"
            | "Outport"
            | "GotoTagVisibility"
            | "DataStoreMemory"
            | "ArgIn"
            | "ArgOut"
            | "PMIOPort"
    ) {
        return Some(format!(
            "{} blocks cannot be commented (Commented={})",
            block.block_type,
            value(setting)
        ));
    }
    if setting == Commented::Out {
        return None;
    }
    let Some(ports) = signal_ports(block) else {
        return Some("Commented=through requires known input and output port counts".into());
    };
    if ports.enable
        + ports.trigger
        + ports.state
        + ports.lconn
        + ports.rconn
        + ports.ifaction
        + ports.reset
        > 0
    {
        return Some(
            "Commented=through requires a block without control or connection ports".into(),
        );
    }
    if ports.inputs != ports.outputs {
        return Some(format!(
            "Commented=through requires equal input and output port counts, but the block has {} inputs and {} outputs",
            ports.inputs, ports.outputs
        ));
    }
    None
}

/// The parameter value a setting is written as.
pub fn value(setting: Commented) -> &'static str {
    match setting {
        Commented::Off => "off",
        Commented::Out => "on",
        Commented::Through => "through",
    }
}

/// Port counts as stored. Blocks created without a `Ports` vector fall back
/// to their boundary ports (subsystems) or the catalog.
fn signal_ports(block: &Block) -> Option<PortCounts> {
    if block.ports != PortCounts::default() {
        return Some(block.ports);
    }
    if let Some(sub) = block.subsystem.as_deref() {
        let count = |kind: &str| sub.blocks.iter().filter(|b| b.block_type == kind).count() as u32;
        return Some(PortCounts::from_slice(&[count("Inport"), count("Outport")]));
    }
    match catalog::find(&block.block_type)?.resolve_ports(&block.parameters) {
        catalog::PortResolution::Known(ports) => Some(ports),
        _ => None,
    }
}

/// Remove commented blocks from every level, grounding or wiring through the
/// signals they carried.
pub(super) fn lower(mut model: Model) -> Result<Model, Error> {
    let mut remaining = 100_000usize;
    system(&mut model.root, 0, &mut remaining)?;
    Ok(model)
}

fn system(sys: &mut System, depth: usize, remaining: &mut usize) -> Result<(), Error> {
    if depth > 64 {
        return Err(Error::Options("subsystem depth exceeds 64".into()));
    }
    *remaining = remaining
        .checked_sub(sys.blocks.len())
        .ok_or_else(|| Error::Options("commented block budget exceeded".into()))?;
    let mut settings = BTreeMap::new();
    for block in &sys.blocks {
        if let Some(reason) = refusal(block) {
            return Err(block_error(&block.id.0, reason));
        }
        match block.commented() {
            Ok(Commented::Off) | Err(_) => {}
            Ok(setting) => {
                settings.insert(block.id.clone(), setting);
            }
        }
    }
    if !settings.is_empty() {
        rewire(sys, &settings)?;
    }
    for block in &mut sys.blocks {
        if let Some(child) = block.subsystem.as_mut() {
            system(child, depth + 1, remaining)?;
        }
    }
    Ok(())
}

/// What a destination ends up reading once commented blocks are gone.
enum Source {
    Driver(Endpoint),
    /// Ground, standing in for a commented-out block (`None`) or for the
    /// undriven input of a commented-through block.
    Ground(BlockId, Option<u32>),
}

fn rewire(sys: &mut System, settings: &BTreeMap<BlockId, Commented>) -> Result<(), Error> {
    let connections = sys.connections();
    // The signal driving each commented block's input port.
    let mut drivers: BTreeMap<(BlockId, u32), Endpoint> = BTreeMap::new();
    for c in &connections {
        if settings.contains_key(&c.dst.block)
            && c.dst.port.kind == PortKind::In
            && drivers
                .insert((c.dst.block.clone(), c.dst.port.index), c.src.clone())
                .is_some()
        {
            return Err(Error::Connection(format!(
                "multiple drivers of commented block {} input {}",
                c.dst.block, c.dst.port.index
            )));
        }
    }
    let mut steps = 1_000_000usize;
    let mut grounds = Grounds::new(sys);
    let mut lines = Vec::with_capacity(connections.len());
    for c in &connections {
        if settings.contains_key(&c.dst.block) {
            // Terminated, or already recorded as a driver.
            continue;
        }
        let src = match resolve(&c.src, settings, &drivers, &mut steps)? {
            Source::Driver(src) => src,
            Source::Ground(from, port) => grounds.endpoint(sys, from, port),
        };
        lines.push(Line {
            src: Some(src),
            dst: Some(c.dst.clone()),
            ..Line::default()
        });
    }
    sys.lines = lines;
    sys.blocks.retain(|b| !settings.contains_key(&b.id));
    sys.blocks.extend(grounds.blocks.into_values());
    Ok(())
}

/// Follow a source through commented-through blocks to the signal that really
/// drives it, or to the ground it reads instead.
fn resolve(
    src: &Endpoint,
    settings: &BTreeMap<BlockId, Commented>,
    drivers: &BTreeMap<(BlockId, u32), Endpoint>,
    steps: &mut usize,
) -> Result<Source, Error> {
    let mut current = src.clone();
    let mut visiting = BTreeSet::new();
    loop {
        *steps = steps
            .checked_sub(1)
            .ok_or_else(|| Error::Options("commented-through chain budget exceeded".into()))?;
        match settings.get(&current.block) {
            None | Some(Commented::Off) => return Ok(Source::Driver(current)),
            Some(Commented::Out) => return Ok(Source::Ground(current.block, None)),
            Some(Commented::Through) => {
                if !visiting.insert(current.block.clone()) {
                    return Err(block_error(
                        &current.block.0,
                        "commented-through blocks form a loop with no driving signal",
                    ));
                }
                match drivers.get(&(current.block.clone(), current.port.index)) {
                    Some(driver) => current = driver.clone(),
                    None => return Ok(Source::Ground(current.block, Some(current.port.index))),
                }
            }
        }
    }
}

/// Ground blocks standing in for removed signals. A commented-out block is
/// replaced by one Ground under its own ID, so its grounded outputs stay
/// addressable by that ID; an undriven commented-through input gets a derived
/// ID that does not collide with any block of the system.
struct Grounds {
    used: BTreeSet<String>,
    ids: BTreeMap<(BlockId, Option<u32>), BlockId>,
    blocks: BTreeMap<BlockId, Block>,
}

impl Grounds {
    fn new(sys: &System) -> Self {
        Grounds {
            used: sys.blocks.iter().map(|b| b.id.0.clone()).collect(),
            ids: BTreeMap::new(),
            blocks: BTreeMap::new(),
        }
    }

    fn endpoint(&mut self, sys: &System, from: BlockId, port: Option<u32>) -> Endpoint {
        let id = match self.ids.get(&(from.clone(), port)) {
            Some(id) => id.clone(),
            None => {
                let id = match port {
                    None => from.clone(),
                    Some(port) => {
                        let mut candidate = format!("{}.commented.in{port}", from.0);
                        while self.used.contains(&candidate) {
                            candidate.push('_');
                        }
                        self.used.insert(candidate.clone());
                        BlockId(candidate)
                    }
                };
                let original = sys.block(&from);
                let name = match (original, port) {
                    (Some(b), None) => b.name.clone(),
                    (Some(b), Some(port)) => format!("{} input {port}", b.name),
                    (None, _) => id.0.clone(),
                };
                self.blocks.insert(
                    id.clone(),
                    Block {
                        id: id.clone(),
                        block_type: "Ground".into(),
                        name,
                        position: original.map(|b| b.position).unwrap_or_default(),
                        orientation: original
                            .map(|b| b.orientation)
                            .unwrap_or(unlinked_model::Orientation::Right),
                        mirrored: false,
                        ports: PortCounts::from_slice(&[0, 1]),
                        parameters: BTreeMap::new(),
                        mask: None,
                        library_source: None,
                        subsystem: None,
                        style: original.map(|b| b.style.clone()).unwrap_or_default(),
                        interface: None,
                    },
                );
                self.ids.insert((from, port), id.clone());
                id
            }
        };
        Endpoint {
            block: id,
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        }
    }
}
