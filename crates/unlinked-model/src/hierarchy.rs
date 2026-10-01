//! Conservative virtual-subsystem creation and lossless-writer recipes.
use crate::catalog::{self, PortResolution};
use crate::edit::{next_sid, system_names, EditError, SystemRef, SID_WATERMARK};
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// A recipe addresses roots in the original parent system, before any moves.
/// Writers clone that raw root, prune destinations, replace its source, and
/// append fresh terminal branches. Unknown surviving content stays on its root.
#[derive(Debug, Clone, PartialEq)]
pub enum LineRecipe {
    Keep {
        original_index: usize,
    },
    Rewrite {
        original_index: usize,
        keep_destinations: Vec<Endpoint>,
        source: Endpoint,
        append_destinations: Vec<Endpoint>,
        clear_points: bool,
    },
}

#[derive(Debug, Clone)]
pub struct CreatePlan {
    /// Selected blocks in their original file order.
    pub selected_indices: Vec<usize>,
    /// Complete resulting wrapper, including moved blocks, ports and lines.
    pub wrapper: Block,
    pub generated_ports: Vec<Block>,
    /// Legacy MDL path IDs gain persistent numeric SIDs when moved.
    pub id_remap: Vec<(BlockId, BlockId)>,
    pub parent_lines: Vec<LineRecipe>,
    pub child_lines: Vec<LineRecipe>,
    pub watermark: u64,
}

fn invalid(message: impl Into<String>) -> EditError {
    EditError::Invalid(message.into())
}

fn system<'a>(model: &'a Model, path: &[BlockId]) -> Result<&'a System, EditError> {
    let mut sys = &model.root;
    for id in path {
        let mut matching = sys.blocks.iter().filter(|b| &b.id == id);
        let block = matching
            .next()
            .ok_or_else(|| EditError::NoSystem(path.to_vec()))?;
        if matching.next().is_some() {
            return Err(invalid("ambiguous subsystem ID"));
        }
        sys = block
            .subsystem
            .as_deref()
            .ok_or_else(|| EditError::NoSystem(path.to_vec()))?;
    }
    Ok(sys)
}

// These mode selectors are serialized even for ordinary Variant=off blocks.
pub(crate) fn variant_key(key: &str) -> bool {
    key.starts_with("Variant") && !matches!(key, "VariantControlMode" | "VariantActivationTime")
}

pub(crate) fn safe_selected(block: &Block) -> Result<(), EditError> {
    let bad = |reason: &str| {
        invalid(format!(
            "cannot move {:?} into a subsystem: {reason}",
            block.name
        ))
    };
    let descriptor =
        catalog::find(&block.block_type).ok_or_else(|| bad("not a supported native leaf block"))?;
    if block.mask.is_some()
        || block.library_source.is_some()
        || block.subsystem.is_some()
        || block.interface.is_some()
        || matches!(
            block.block_type.as_str(),
            "Inport" | "Outport" | "Goto" | "From" | "GotoTagVisibility"
        )
    {
        return Err(bad(
            "hierarchical, linked, masked, interface and scoped blocks are unsupported",
        ));
    }
    for (key, value) in &block.parameters {
        let value = value.trim();
        if (key.ends_with("Fcn") && !value.is_empty())
            || (key == "Commented" && !matches!(value, "" | "off"))
            || ((variant_key(key) || key.starts_with("Mask") || key == "LinkStatus")
                && !matches!(value, "" | "off" | "none"))
        {
            return Err(bad(&format!("scope-sensitive parameter {key}")));
        }
    }
    let PortResolution::Known(ports) = descriptor.resolve_ports(&block.parameters) else {
        return Err(bad("port counts are unresolved or invalid"));
    };
    if ports != block.ports
        || ports.enable != 0
        || ports.trigger != 0
        || ports.state != 0
        || ports.lconn != 0
        || ports.rconn != 0
        || ports.ifaction != 0
        || ports.reset != 0
    {
        return Err(bad(
            "only resolved ordinary input/output ports are supported",
        ));
    }
    let p = block.position;
    if [p.left, p.top, p.right, p.bottom]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 1e9)
        || p.left >= p.right
        || p.top >= p.bottom
    {
        return Err(bad("invalid block geometry"));
    }
    Ok(())
}

pub(crate) fn destinations(line: &Line) -> (Vec<Endpoint>, bool) {
    let mut out = Vec::new();
    if let Some(dst) = &line.dst {
        out.push(dst.clone());
    }
    let mut complete = line.dst.is_some() || !line.branches.is_empty();
    let mut stack: Vec<_> = line.branches.iter().collect();
    while let Some(branch) = stack.pop() {
        if let Some(dst) = &branch.dst {
            out.push(dst.clone());
        }
        complete &= branch.dst.is_some() || !branch.branches.is_empty();
        stack.extend(&branch.branches);
    }
    (out, complete)
}

fn endpoint(block: &BlockId, kind: PortKind, index: u32) -> Endpoint {
    Endpoint {
        block: block.clone(),
        port: PortRef { kind, index },
    }
}

pub(crate) fn validate_endpoint(
    blocks: &BTreeMap<&BlockId, &Block>,
    ep: &Endpoint,
    source: bool,
) -> Result<(), EditError> {
    let b = blocks
        .get(&ep.block)
        .ok_or_else(|| EditError::NoBlock(ep.block.clone()))?;
    if ep.port.kind != if source { PortKind::Out } else { PortKind::In }
        || ep.port.index == 0
        || ep.port.index > b.ports.count(ep.port.kind)
    {
        return Err(EditError::NoPort(ep.clone()));
    }
    if b.mask.is_none() && b.library_source.is_none() && b.subsystem.is_none() {
        if let Some(d) = catalog::find(&b.block_type) {
            if d.resolve_ports(&b.parameters) != PortResolution::Known(b.ports) {
                return Err(invalid(
                    "boundary endpoint ports are unresolved or inconsistent",
                ));
            }
        }
    }
    Ok(())
}

fn sorted_sources(
    blocks: &BTreeMap<&BlockId, &Block>,
    sources: BTreeSet<Endpoint>,
) -> Vec<Endpoint> {
    let mut sources: Vec<_> = sources.into_iter().collect();
    sources.sort_by(|a, b| {
        let a_pos = blocks[&a.block].position;
        let b_pos = blocks[&b.block].position;
        a_pos
            .top
            .total_cmp(&b_pos.top)
            .then(a_pos.left.total_cmp(&b_pos.left))
            .then(a.cmp(b))
    });
    sources
}

// Boundary blocks must carry inherited signal semantics even when a file has
// customized per-type defaults. Preserve other defaults in the preview and file.
fn generated_parameters(model: &Model, kind: &str) -> Result<BTreeMap<String, String>, EditError> {
    let mut p = model.type_defaults.get(kind).cloned().unwrap_or_default();
    for (key, value) in &p {
        if (key.ends_with("Fcn") && !value.trim().is_empty())
            || ((key.starts_with("Mask") || variant_key(key) || key == "LinkStatus")
                && !matches!(value.trim(), "" | "off" | "none"))
        {
            return Err(invalid(format!(
                "unsupported {kind} document default {key}"
            )));
        }
    }
    let defaults: &[(&str, &str)] = if kind == "SubSystem" {
        &[
            ("TreatAsAtomicUnit", "off"),
            ("SystemSampleTime", "-1"),
            ("SFBlockType", "NONE"),
            ("SimViewingDevice", "off"),
            ("PermitHierarchicalResolution", "All"),
            ("Commented", "off"),
        ]
    } else {
        &[
            ("SampleTime", "-1"),
            ("PortDimensions", "-1"),
            ("OutDataTypeStr", "Inherit: auto"),
            ("SignalType", "auto"),
            ("SamplingMode", "auto"),
            ("VarSizeSig", "Inherit"),
            ("Unit", "inherit"),
            ("OutMin", "[]"),
            ("OutMax", "[]"),
            ("BusOutputAsStruct", "off"),
            ("Commented", "off"),
        ]
    };
    // Absent parameters use the source release's factory defaults. Do not add
    // newer parameter names to old files just to restate those defaults.
    for (key, value) in defaults {
        if let Some(existing) = p.get_mut(*key) {
            *existing = value.to_string();
        }
    }
    if kind == "Inport" {
        for key in [
            "OutputFunctionCall",
            "LatchInputForFeedbackSignals",
            "LatchByDelayingOutsideSignal",
        ] {
            if let Some(existing) = p.get_mut(key) {
                *existing = "off".into();
            }
        }
    }
    Ok(p)
}

/// Plan without mutating input. Selection order does not affect port numbering.
/// Ordinary named nets may move intact, but named boundary nets are refused
/// until both sides' signal-label semantics can be represented explicitly.
pub fn plan_create(
    model: &Model,
    path: &SystemRef,
    ids: &[BlockId],
    id: &BlockId,
    name: &str,
) -> Result<CreatePlan, EditError> {
    if ids.is_empty() || ids.len() > 256 {
        return Err(invalid("subsystem selection must contain 1..=256 blocks"));
    }
    if name.trim().is_empty()
        || name.len() > 1024
        || name.chars().any(|c| {
            (c < ' ' && !matches!(c, '\n' | '\r' | '\t')) || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(invalid("invalid subsystem name"));
    }
    if crate::validation::validate_structure(model).truncated {
        return Err(invalid("model exceeds hierarchy validation budget"));
    }
    let sid =
        id.0.parse::<u64>()
            .ok()
            .filter(|sid| sid.to_string() == id.0)
            .ok_or_else(|| invalid("wrapper SID must be a canonical integer"))?;
    if sid < next_sid(model).ok_or_else(|| invalid("no SIDs left"))? {
        return Err(invalid("wrapper SID is already reserved"));
    }
    let mut ancestor = &model.root;
    for ancestor_id in path {
        let b = ancestor
            .block(ancestor_id)
            .ok_or_else(|| EditError::NoSystem(path.clone()))?;
        if b.mask.is_some()
            || b.library_source.is_some()
            || b.interface.is_some()
            || b.block_type != "SubSystem"
            || b.parameters.iter().any(|(k, v)| {
                (k == "TreatAsAtomicUnit" && v != "off")
                    || (k == "SystemSampleTime" && v != "-1")
                    || ((variant_key(k) || k.starts_with("Mask") || k.ends_with("Fcn"))
                        && !matches!(v.trim(), "" | "off" | "none"))
            })
            || b.ports.enable != 0
            || b.ports.trigger != 0
            || b.ports.reset != 0
            || b.ports.ifaction != 0
            || b.ports.lconn != 0
            || b.ports.rconn != 0
            || b.ports.state != 0
        {
            return Err(invalid("grouping inside a masked, linked, conditional, atomic or scoped subsystem is unsupported"));
        }
        ancestor = b
            .subsystem
            .as_deref()
            .ok_or_else(|| EditError::NoSystem(path.clone()))?;
    }
    let sys = system(model, path)?;
    let mut block_index = BTreeMap::new();
    for block in &sys.blocks {
        if block_index.insert(&block.id, block).is_some() {
            return Err(invalid("ambiguous block IDs in the target system"));
        }
    }
    if sys.blocks.iter().any(|b| b.name == name) {
        return Err(EditError::DuplicateName(name.into()));
    }
    let selected: BTreeSet<_> = ids.iter().cloned().collect();
    if selected.len() != ids.len() {
        return Err(invalid("repeated selected block ID"));
    }
    let selected_indices: Vec<_> = sys
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| selected.contains(&b.id))
        .map(|(i, _)| i)
        .collect();
    if selected_indices.len() != ids.len() {
        return Err(invalid("selection contains missing or ambiguous IDs"));
    }
    for selected_id in &selected {
        if sys.blocks.iter().filter(|b| &b.id == selected_id).count() != 1 {
            return Err(invalid("selected block ID is missing or ambiguous"));
        }
    }
    let names = system_names(model, path).ok_or_else(|| EditError::NoSystem(path.clone()))?;
    for end in 1..=names.len() {
        let ancestor_path = names[..end]
            .iter()
            .map(|n| n.replace('/', "//"))
            .collect::<Vec<_>>()
            .join("/");
        if model.charts.iter().any(|c| c.name == ancestor_path) {
            return Err(invalid(
                "grouping inside a chart-owning subsystem is unsupported",
            ));
        }
    }
    let mut bounds: Option<Rect> = None;
    let mut id_remap = Vec::new();
    for &i in &selected_indices {
        let old = &sys.blocks[i].id;
        if old.0.parse::<u64>().is_err() {
            if model.source == SourceFormat::Slx {
                return Err(invalid("SLX selected blocks need numeric SIDs"));
            }
            let new = sid
                .checked_add(id_remap.len() as u64 + 1)
                .ok_or_else(|| invalid("SID allocation overflows"))?;
            id_remap.push((old.clone(), BlockId(new.to_string())));
        }
    }
    let mapped: BTreeMap<_, _> = id_remap.iter().cloned().collect();
    for &i in &selected_indices {
        let b = &sys.blocks[i];
        safe_selected(b)?;
        let full = names
            .iter()
            .chain(std::iter::once(&b.name))
            .map(|n| n.replace('/', "//"))
            .collect::<Vec<_>>()
            .join("/");
        if model
            .charts
            .iter()
            .any(|c| c.name == full || c.name.starts_with(&(full.clone() + "/")))
        {
            return Err(invalid("selection owns a chart"));
        }
        bounds = Some(bounds.map_or(b.position, |r| r.union(&b.position)));
    }
    // Analyze every root once, retaining untouched roots (even dangling ones).
    let mut analyzed = Vec::new();
    let mut incoming = BTreeSet::new();
    let mut outgoing = BTreeSet::new();
    let mut source_counts = BTreeMap::<Endpoint, usize>::new();
    let mut drivers = BTreeMap::<Endpoint, usize>::new();
    for line in &sys.lines {
        if let Some(src) = &line.src {
            *source_counts.entry(src.clone()).or_default() += 1;
        }
        let (dests, complete) = destinations(line);
        if line.src.is_none() || !complete {
            return Err(invalid(
                "detached or dangling nets in the system are unsupported for grouping",
            ));
        }
        for dst in &dests {
            *drivers.entry(dst.clone()).or_default() += 1;
        }
        let touched = line
            .src
            .as_ref()
            .is_some_and(|s| selected.contains(&s.block))
            || dests.iter().any(|d| selected.contains(&d.block));
        analyzed.push((dests, complete, touched));
    }
    for (line, (dests, complete, touched)) in sys.lines.iter().zip(&analyzed) {
        if !touched {
            continue;
        }
        let src = line
            .src
            .as_ref()
            .ok_or_else(|| invalid("selected block has a sourceless line"))?;
        if !complete || source_counts[src] != 1 {
            return Err(invalid(
                "touched roots must be complete and uniquely sourced",
            ));
        }
        validate_endpoint(&block_index, src, true)?;
        for dst in dests {
            validate_endpoint(&block_index, dst, false)?;
            if drivers[dst] != 1 {
                return Err(invalid("touched input has ambiguous drivers"));
            }
        }
        let inside = selected.contains(&src.block);
        let crosses = dests.iter().any(|d| selected.contains(&d.block) != inside);
        if crosses && line.name.as_ref().is_some_and(|n| !n.is_empty()) {
            return Err(invalid(
                "named boundary nets cannot be moved into a subsystem yet",
            ));
        }
        if !inside {
            incoming.insert(src.clone());
        } else if crosses {
            outgoing.insert(src.clone());
        }
    }
    let incoming = sorted_sources(&block_index, incoming);
    let outgoing = sorted_sources(&block_index, outgoing);
    if incoming.len() + outgoing.len() > 1024 {
        return Err(invalid("subsystem exceeds 1024 boundary ports"));
    }
    let watermark = sid
        .checked_add((id_remap.len() + incoming.len() + outgoing.len()) as u64)
        .ok_or_else(|| invalid("subsystem SID allocation overflows"))?;
    let bounds = bounds.unwrap();
    let center = Point {
        x: (bounds.left + bounds.right) / 2.,
        y: (bounds.top + bounds.bottom) / 2.,
    };
    let mut generated_ports = Vec::new();
    let mut names: BTreeSet<_> = selected_indices
        .iter()
        .map(|&i| sys.blocks[i].name.clone())
        .collect();
    let mut input_map = BTreeMap::new();
    let mut output_map = BTreeMap::new();
    for (inputs, sources) in [(true, &incoming), (false, &outgoing)] {
        for (i, src) in sources.iter().enumerate() {
            let kind = if inputs { "Inport" } else { "Outport" };
            let mut name = format!("{}{}", if inputs { "In" } else { "Out" }, i + 1);
            while !names.insert(name.clone()) {
                name.push('_');
            }
            let new_id = BlockId(
                (sid + id_remap.len() as u64 + generated_ports.len() as u64 + 1).to_string(),
            );
            let descriptor = catalog::find(kind).unwrap();
            let mut parameters = generated_parameters(model, kind)?;
            parameters.extend(descriptor.creation_parameters());
            parameters.insert("Port".into(), (i + 1).to_string());
            let PortResolution::Known(ports) = descriptor.resolve_ports(&parameters) else {
                unreachable!()
            };
            let x = if inputs {
                bounds.left - 70.
            } else {
                bounds.right + 40.
            };
            let y = bounds.top + i as f64 * 30.;
            if [x, y, x + 30., y + 14.].iter().any(|v| v.abs() > 1e9) {
                return Err(invalid(
                    "generated port position would exceed canvas bounds",
                ));
            }
            generated_ports.push(Block {
                id: new_id.clone(),
                block_type: kind.into(),
                name,
                position: Rect::new(x, y, x + 30., y + 14.),
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
            if inputs {
                input_map.insert(src.clone(), (new_id, i as u32 + 1));
            } else {
                output_map.insert(src.clone(), (new_id, i as u32 + 1));
            }
        }
    }
    let mut parent_lines = Vec::new();
    let mut child_lines = Vec::new();
    for (index, (line, (dests, _, touched))) in sys.lines.iter().zip(&analyzed).enumerate() {
        if !touched {
            parent_lines.push(LineRecipe::Keep {
                original_index: index,
            });
            continue;
        }
        let src = line.src.as_ref().unwrap();
        let (inside, outside): (Vec<_>, Vec<_>) = dests
            .iter()
            .cloned()
            .partition(|d| selected.contains(&d.block));
        let rewrite = |keep_destinations, source, append_destinations| LineRecipe::Rewrite {
            original_index: index,
            keep_destinations,
            source,
            append_destinations,
            clear_points: true,
        };
        if selected.contains(&src.block) {
            if outside.is_empty() {
                child_lines.push(LineRecipe::Keep {
                    original_index: index,
                });
            } else {
                let (port, number) = &output_map[src];
                child_lines.push(rewrite(
                    inside,
                    src.clone(),
                    vec![endpoint(port, PortKind::In, 1)],
                ));
                parent_lines.push(rewrite(
                    outside,
                    endpoint(id, PortKind::Out, *number),
                    vec![],
                ));
            }
        } else {
            let (port, number) = &input_map[src];
            parent_lines.push(rewrite(
                outside,
                src.clone(),
                vec![endpoint(id, PortKind::In, *number)],
            ));
            child_lines.push(rewrite(inside, endpoint(port, PortKind::Out, 1), vec![]));
        }
    }
    let mut blocks: Vec<_> = selected_indices
        .iter()
        .map(|&i| sys.blocks[i].clone())
        .collect();
    for block in &mut blocks {
        if let Some(id) = mapped.get(&block.id) {
            block.id = id.clone();
        }
    }
    blocks.extend(generated_ports.iter().cloned());
    let mut child = System {
        blocks,
        lines: materialize_lines(sys, &child_lines),
        annotations: vec![],
        properties: BTreeMap::new(),
    };
    fn remap_endpoint(ep: &mut Option<Endpoint>, mapped: &BTreeMap<BlockId, BlockId>) {
        if let Some(ep) = ep {
            if let Some(id) = mapped.get(&ep.block) {
                ep.block = id.clone();
            }
        }
    }
    fn remap_branches(branches: &mut [Branch], mapped: &BTreeMap<BlockId, BlockId>) {
        for branch in branches {
            remap_endpoint(&mut branch.dst, mapped);
            remap_branches(&mut branch.branches, mapped);
        }
    }
    for line in &mut child.lines {
        remap_endpoint(&mut line.src, &mapped);
        remap_endpoint(&mut line.dst, &mapped);
        remap_branches(&mut line.branches, &mapped);
    }
    for recipe in &mut child_lines {
        if let LineRecipe::Rewrite { source, .. } = recipe {
            if let Some(id) = mapped.get(&source.block) {
                source.block = id.clone();
            }
        }
    }
    let half_height = (incoming.len().max(outgoing.len()) as f64 * 14.).max(30.);
    let wrapper_position = Rect::new(
        center.x - 40.,
        center.y - half_height,
        center.x + 40.,
        center.y + half_height,
    );
    if [
        wrapper_position.left,
        wrapper_position.top,
        wrapper_position.right,
        wrapper_position.bottom,
    ]
    .iter()
    .any(|v| v.abs() > 1e9)
    {
        return Err(invalid("subsystem position exceeds canvas bounds"));
    }
    let wrapper = Block {
        id: id.clone(),
        block_type: "SubSystem".into(),
        name: name.into(),
        position: wrapper_position,
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts {
            inputs: incoming.len() as u32,
            outputs: outgoing.len() as u32,
            ..Default::default()
        },
        parameters: generated_parameters(model, "SubSystem")?,
        mask: None,
        library_source: None,
        subsystem: Some(Box::new(child)),
        style: BlockStyle::default(),
        interface: None,
    };
    Ok(CreatePlan {
        selected_indices,
        wrapper,
        generated_ports,
        id_remap,
        parent_lines,
        child_lines,
        watermark,
    })
}

/// Apply clone/prune recipes to the IR; writers perform the same operations on
/// raw line records instead of regenerating their unmodeled properties.
pub fn materialize_lines(original: &System, recipes: &[LineRecipe]) -> Vec<Line> {
    fn prune(
        dst: &mut Option<Endpoint>,
        branches: &mut Vec<Branch>,
        keep: &BTreeSet<&Endpoint>,
        clear: bool,
    ) {
        if dst.as_ref().is_some_and(|d| !keep.contains(d)) {
            *dst = None;
        }
        branches.retain_mut(|b| {
            prune(&mut b.dst, &mut b.branches, keep, clear);
            if clear {
                b.points.clear();
            }
            b.dst.is_some() || !b.branches.is_empty()
        });
    }
    recipes
        .iter()
        .map(|r| match r {
            LineRecipe::Keep { original_index } => original.lines[*original_index].clone(),
            LineRecipe::Rewrite {
                original_index,
                keep_destinations,
                source,
                append_destinations,
                clear_points,
            } => {
                let mut line = original.lines[*original_index].clone();
                let keep = keep_destinations.iter().collect();
                prune(&mut line.dst, &mut line.branches, &keep, *clear_points);
                line.src = Some(source.clone());
                if *clear_points {
                    line.points.clear();
                }
                if !append_destinations.is_empty() {
                    if let Some(dst) = line.dst.take() {
                        line.branches.insert(
                            0,
                            Branch {
                                dst: Some(dst),
                                ..Default::default()
                            },
                        );
                    }
                }
                for dst in append_destinations {
                    line.branches.push(Branch {
                        dst: Some(dst.clone()),
                        ..Default::default()
                    });
                }
                line
            }
        })
        .collect()
}

pub fn apply_create(
    model: &mut Model,
    path: &SystemRef,
    ids: &[BlockId],
    id: &BlockId,
    name: &str,
) -> Result<(), EditError> {
    let plan = plan_create(model, path, ids, id, name)?;
    let mut sys = &mut model.root;
    for id in path {
        sys = sys
            .blocks
            .iter_mut()
            .find(|b| &b.id == id)
            .and_then(|b| b.subsystem.as_deref_mut())
            .ok_or_else(|| EditError::NoSystem(path.clone()))?;
    }
    let lines = materialize_lines(sys, &plan.parent_lines);
    let selected: BTreeSet<_> = plan.selected_indices.into_iter().collect();
    let mut index = 0;
    sys.blocks.retain(|_| {
        let keep = !selected.contains(&index);
        index += 1;
        keep
    });
    sys.blocks.push(plan.wrapper);
    sys.lines = lines;
    model
        .root
        .properties
        .insert(SID_WATERMARK.into(), plan.watermark.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{apply_batch, Edit};
    fn model() -> Model {
        let mut m = Model {
            name: "group".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: SimConfig::default(),
            root: System::default(),
            workspace: BTreeMap::new(),
            type_defaults: Default::default(),
            charts: vec![],
        };
        for (id, kind, x, y) in [
            ("1", "Constant", 0., 0.),
            ("2", "Gain", 100., 0.),
            ("3", "Gain", 200., 0.),
            ("4", "Gain", 300., 0.),
            ("5", "Gain", 100., 100.),
        ] {
            Edit::AddBlock {
                system: vec![],
                id: id.into(),
                block_type: kind.into(),
                name: format!("block{id}"),
                position: Rect::new(x, y, x + 30., y + 30.),
            }
            .apply(&mut m)
            .unwrap();
        }
        for (src, dst) in [("1", "2"), ("1", "5"), ("2", "3"), ("3", "4")] {
            Edit::Connect {
                system: vec![],
                src: endpoint(&src.into(), PortKind::Out, 1),
                dst: endpoint(&dst.into(), PortKind::In, 1),
            }
            .apply(&mut m)
            .unwrap();
        }
        m
    }
    fn group(m: &mut Model, ids: &[&str]) -> Result<(), crate::edit::BatchError> {
        apply_batch(
            m,
            &[Edit::CreateSubsystem {
                system: vec![],
                ids: ids.iter().map(|s| BlockId::from(*s)).collect(),
                id: "10".into(),
                name: "grouped".into(),
            }],
        )
    }
    #[test]
    fn partitions_fanout_and_keeps_internal_labels_and_geometry() {
        let mut m = model();
        let internal = m
            .root
            .lines
            .iter_mut()
            .find(|l| l.src.as_ref().unwrap().block.0 == "2")
            .unwrap();
        internal.name = Some("inside".into());
        internal.points = vec![Point { x: 150., y: 15. }];
        let old = m.clone();
        group(&mut m, &["2", "3"]).unwrap();
        let wrapper = m.root.block(&"10".into()).unwrap();
        assert_eq!(wrapper.ports.inputs, 1);
        assert_eq!(wrapper.ports.outputs, 1);
        assert_eq!(wrapper.position, Rect::new(125., -15., 205., 45.));
        let child = wrapper.subsystem.as_deref().unwrap();
        assert_eq!(
            child.block(&"2".into()).unwrap(),
            old.root.block(&"2".into()).unwrap()
        );
        let line = child
            .lines
            .iter()
            .find(|l| l.name.as_deref() == Some("inside"))
            .unwrap();
        assert_eq!(line.points, vec![Point { x: 150., y: 15. }]);
        let incoming = m.root.connections();
        assert!(incoming
            .iter()
            .any(|c| c.src.block.0 == "1" && c.dst.block.0 == "5"));
        assert!(incoming
            .iter()
            .any(|c| c.src.block.0 == "1" && c.dst.block.0 == "10"));
        assert!(incoming
            .iter()
            .any(|c| c.src.block.0 == "10" && c.dst.block.0 == "4"));
        assert_eq!(next_sid(&m), Some(13));
    }
    #[test]
    fn multiple_inside_destinations_share_one_input_port_and_keep_nested_shape() {
        let mut m = model();
        let line = &mut m.root.lines[0];
        let children = std::mem::take(&mut line.branches);
        line.branches = vec![Branch {
            branches: children,
            points: vec![Point { x: 50., y: 50. }],
            ..Default::default()
        }];
        group(&mut m, &["2", "5"]).unwrap();
        let wrapper = m.root.block(&"10".into()).unwrap();
        assert_eq!(wrapper.ports.inputs, 1);
        let child = wrapper.subsystem.as_deref().unwrap();
        let input = child
            .lines
            .iter()
            .find(|l| l.src.as_ref().unwrap().block.0 == "11")
            .unwrap();
        assert_eq!(input.branches.len(), 1);
        assert_eq!(input.branches[0].branches.len(), 2);
        assert!(input.branches[0].points.is_empty());
    }
    #[test]
    fn legacy_path_ids_get_persistent_sids_and_safe_port_names() {
        let mut m = model();
        m.root.blocks[1].id = "path:old/gain".into();
        m.root.blocks[1].name = "In1".into();
        for line in &mut m.root.lines {
            if line.src.as_ref().is_some_and(|e| e.block.0 == "2") {
                line.src.as_mut().unwrap().block = "path:old/gain".into();
            }
            for branch in &mut line.branches {
                if branch.dst.as_ref().is_some_and(|e| e.block.0 == "2") {
                    branch.dst.as_mut().unwrap().block = "path:old/gain".into();
                }
            }
            if line.dst.as_ref().is_some_and(|e| e.block.0 == "2") {
                line.dst.as_mut().unwrap().block = "path:old/gain".into();
            }
        }
        let plan = plan_create(
            &m,
            &vec![],
            &["path:old/gain".into()],
            &"10".into(),
            "grouped",
        )
        .unwrap();
        assert_eq!(plan.id_remap, vec![("path:old/gain".into(), "11".into())]);
        assert_eq!(plan.generated_ports[0].name, "In1_");
        group(&mut m, &["path:old/gain"]).unwrap();
        let child = m
            .root
            .block(&"10".into())
            .unwrap()
            .subsystem
            .as_deref()
            .unwrap();
        assert!(child.block(&"11".into()).is_some());
        assert!(child
            .connections()
            .iter()
            .all(|c| !c.src.block.0.starts_with("path:") && !c.dst.block.0.starts_with("path:")));
    }
    #[test]
    fn unsafe_operations_refuse_without_mutation() {
        let mut m = model();
        m.root.lines[0].name = Some("boundary".into());
        let before = m.clone();
        assert!(group(&mut m, &["2"]).is_err());
        assert_eq!(m, before);
        m.root.lines[0].name = None;
        m.root.blocks[1]
            .parameters
            .insert("InitFcn".into(), "doStuff".into());
        assert!(group(&mut m, &["2"]).is_err());
        m.root.blocks[1].parameters.remove("InitFcn");
        m.root.lines.push(Line::default());
        assert!(group(&mut m, &["2"]).is_err());
        m.root.lines.pop();
        assert!(plan_create(
            &m,
            &vec![],
            &["2".into()],
            &u64::MAX.to_string().as_str().into(),
            "grouped"
        )
        .is_err());
    }
    #[test]
    fn selection_order_does_not_change_port_order() {
        let m = model();
        let a = plan_create(
            &m,
            &vec![],
            &["2".into(), "3".into()],
            &"10".into(),
            "grouped",
        )
        .unwrap();
        let b = plan_create(
            &m,
            &vec![],
            &["3".into(), "2".into()],
            &"10".into(),
            "grouped",
        )
        .unwrap();
        assert_eq!(a.wrapper, b.wrapper);
        assert_eq!(a.parent_lines, b.parent_lines);
    }
}
