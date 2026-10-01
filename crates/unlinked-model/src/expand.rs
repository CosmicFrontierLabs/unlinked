//! Conservative expansion of ordinary virtual subsystems. Recipes refer to
//! original trees, so file writers can graft raw branches without regenerating
//! their opaque properties. Writers additionally validate discarded raw metadata.
use crate::edit::{next_sid, system_names, EditError, SystemRef, SID_WATERMARK};
use crate::hierarchy::{destinations, safe_selected, validate_endpoint};
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RootRef {
    Parent(usize),
    Child(usize),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Graft {
    /// Original endpoint to replace at its existing position in the base tree.
    pub destination: Endpoint,
    /// Contributes all destination branches, never its source.
    pub donor: RootRef,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedLineRecipe {
    pub base: RootRef,
    pub grafts: Vec<Graft>,
    pub clear_points: bool,
}
#[derive(Debug, Clone)]
pub struct ExpandPlan {
    pub wrapper_index: usize,
    /// Original child block indices, in original order.
    pub moved_indices: Vec<usize>,
    /// Final remapped IDs and translated geometry, aligned with moved_indices.
    pub moved_blocks: Vec<Block>,
    /// Original names, including removed interface blocks, for legacy raw endpoints.
    pub child_names: BTreeMap<BlockId, String>,
    pub removed_port_ids: Vec<BlockId>,
    pub id_remap: Vec<(BlockId, BlockId)>,
    pub translation: Point,
    pub lines: Vec<ExpandedLineRecipe>,
    pub watermark: u64,
    pub parent_line_count: usize,
    pub child_line_count: usize,
}
fn invalid(message: impl Into<String>) -> EditError {
    EditError::Invalid(message.into())
}
fn ordinary_ports(ports: PortCounts) -> bool {
    ports.enable == 0
        && ports.trigger == 0
        && ports.state == 0
        && ports.lconn == 0
        && ports.rconn == 0
        && ports.ifaction == 0
        && ports.reset == 0
}
fn scoped(block: &Block) -> bool {
    block.mask.is_some()
        || block.library_source.is_some()
        || block.interface.is_some()
        || block.parameters.iter().any(|(k, v)| {
            let v = v.trim();
            (k.ends_with("Fcn") && !v.is_empty())
                || (k == "Commented" && !matches!(v, "" | "off"))
                || ((k.starts_with("Variant") || k.starts_with("Mask") || k == "LinkStatus")
                    && !matches!(v, "" | "off" | "none"))
        })
}
fn plain_wrapper(block: &Block) -> Result<(), EditError> {
    if block.block_type != "SubSystem"
        || block.subsystem.is_none()
        || scoped(block)
        || !ordinary_ports(block.ports)
    {
        return Err(invalid("only plain virtual subsystems can be expanded"));
    }
    for (key, allowed) in [
        ("TreatAsAtomicUnit", "off"),
        ("SystemSampleTime", "-1"),
        ("SFBlockType", "NONE"),
        ("SimViewingDevice", "off"),
        ("PermitHierarchicalResolution", "All"),
    ] {
        if block.param(key).is_some_and(|v| v.trim() != allowed) {
            return Err(invalid(format!("unsupported subsystem parameter {key}")));
        }
    }
    Ok(())
}
fn inherited_port(block: &Block) -> Result<u32, EditError> {
    if scoped(block) || block.subsystem.is_some() || !ordinary_ports(block.ports) {
        return Err(invalid(
            "only inherited ordinary subsystem ports can be removed",
        ));
    }
    let expected = if block.block_type == "Inport" {
        PortCounts {
            outputs: 1,
            ..Default::default()
        }
    } else {
        PortCounts {
            inputs: 1,
            ..Default::default()
        }
    };
    if block.ports != expected {
        return Err(invalid("invalid boundary port counts"));
    }
    for (key, allowed) in [
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
        ("OutputFunctionCall", "off"),
        ("LatchInputForFeedbackSignals", "off"),
        ("LatchByDelayingOutsideSignal", "off"),
        ("MustResolveToSignalObject", "off"),
        ("InitialOutput", "[]"),
        ("OutputWhenDisabled", "held"),
    ] {
        if block.param(key).is_some_and(|v| v.trim() != allowed) {
            return Err(invalid(format!("unsupported boundary parameter {key}")));
        }
    }
    crate::catalog::interface_port_number(block).map_err(invalid)
}
fn block_index(sys: &System) -> Result<BTreeMap<&BlockId, &Block>, EditError> {
    let mut index = BTreeMap::new();
    let mut names = BTreeSet::new();
    for b in &sys.blocks {
        if index.insert(&b.id, b).is_some() || !names.insert(&b.name) {
            return Err(invalid("ambiguous block IDs or names"));
        }
    }
    Ok(index)
}
struct Nets {
    sources: BTreeMap<Endpoint, usize>,
    drivers: BTreeMap<Endpoint, usize>,
    destinations: Vec<Vec<Endpoint>>,
}
fn nets(sys: &System, index: &BTreeMap<&BlockId, &Block>) -> Result<Nets, EditError> {
    let mut result = Nets {
        sources: BTreeMap::new(),
        drivers: BTreeMap::new(),
        destinations: vec![],
    };
    for (i, line) in sys.lines.iter().enumerate() {
        let mut pending: Vec<_> = line.branches.iter().map(|b| (b, 1usize)).collect();
        while let Some((branch, depth)) = pending.pop() {
            if depth > 64 {
                return Err(invalid("expansion branch depth exceeds 64"));
            }
            pending.extend(branch.branches.iter().map(|b| (b, depth + 1)));
        }
        let src = line
            .src
            .as_ref()
            .ok_or_else(|| invalid("sourceless expansion net"))?;
        let (dests, complete) = destinations(line);
        if !complete || result.sources.insert(src.clone(), i).is_some() {
            return Err(invalid("expansion requires complete uniquely sourced nets"));
        }
        validate_endpoint(index, src, true)?;
        for dst in &dests {
            validate_endpoint(index, dst, false)?;
            if result.drivers.insert(dst.clone(), i).is_some() {
                return Err(invalid("ambiguous input driver"));
            }
        }
        result.destinations.push(dests);
    }
    Ok(result)
}
fn ep(id: &BlockId, kind: PortKind, index: u32) -> Endpoint {
    Endpoint {
        block: id.clone(),
        port: PortRef { kind, index },
    }
}
fn named(line: &Line) -> bool {
    line.name.as_ref().is_some_and(|n| !n.is_empty())
}

pub fn plan_expand(model: &Model, path: &SystemRef, id: &BlockId) -> Result<ExpandPlan, EditError> {
    if path.len() > 64 || crate::validation::validate_structure(model).truncated {
        return Err(invalid("model exceeds expansion validation budget"));
    }
    let mut parent = &model.root;
    for ancestor in path {
        let index = block_index(parent)?;
        let block = *index
            .get(ancestor)
            .ok_or_else(|| EditError::NoSystem(path.clone()))?;
        plain_wrapper(block)?;
        parent = block.subsystem.as_deref().unwrap();
    }
    let parent_index = block_index(parent)?;
    let wrapper = *parent_index
        .get(id)
        .ok_or_else(|| EditError::NoBlock(id.clone()))?;
    plain_wrapper(wrapper)?;
    if wrapper.orientation != Orientation::Right || wrapper.mirrored {
        return Err(invalid(
            "expansion requires an unmirrored right-facing wrapper",
        ));
    }
    let names = system_names(model, path).ok_or_else(|| EditError::NoSystem(path.clone()))?;
    let full = names
        .iter()
        .chain(std::iter::once(&wrapper.name))
        .map(|n| n.replace('/', "//"))
        .collect::<Vec<_>>()
        .join("/");
    if model.charts.iter().any(|c| {
        c.name == full
            || c.name.starts_with(&(full.clone() + "/"))
            || (!c.name.is_empty() && full.starts_with(&(c.name.clone() + "/")))
    }) {
        return Err(invalid("chart-owning scope cannot be expanded"));
    }
    let child = wrapper.subsystem.as_deref().unwrap();
    if !child.annotations.is_empty() || !child.properties.is_empty() {
        return Err(invalid(
            "expansion of child annotations or system properties is unsupported",
        ));
    }
    if child.blocks.len() > 1280 || parent.blocks.len() > 100_000 {
        return Err(invalid("expansion block budget exceeded"));
    }
    let parent_names: BTreeSet<_> = parent
        .blocks
        .iter()
        .filter(|b| b.id != *id)
        .map(|b| b.name.as_str())
        .collect();
    let child_index = block_index(child)?;
    if child
        .blocks
        .iter()
        .any(|b| parent_index.contains_key(&b.id))
    {
        return Err(invalid("child IDs collide with parent IDs"));
    }
    let mut inputs = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    let mut moved_indices = vec![];
    let mut bounds: Option<Rect> = None;
    for (i, b) in child.blocks.iter().enumerate() {
        if matches!(b.block_type.as_str(), "Inport" | "Outport") {
            let number = inherited_port(b)?;
            let table = if b.block_type == "Inport" {
                &mut inputs
            } else {
                &mut outputs
            };
            if table.insert(number, b).is_some() {
                return Err(invalid("duplicate boundary port number"));
            }
        } else {
            safe_selected(b)?;
            if parent_names.contains(b.name.as_str()) {
                return Err(invalid("expanded block name or ID collides with parent"));
            }
            moved_indices.push(i);
            bounds = Some(bounds.map_or(b.position, |r| r.union(&b.position)));
        }
    }
    if moved_indices.is_empty() || moved_indices.len() > 256 {
        return Err(invalid("expansion requires 1..=256 native children"));
    }
    if inputs.len() != wrapper.ports.inputs as usize
        || outputs.len() != wrapper.ports.outputs as usize
        || inputs.keys().copied().ne(1..=wrapper.ports.inputs)
        || outputs.keys().copied().ne(1..=wrapper.ports.outputs)
        || inputs.len() + outputs.len() > 1024
    {
        return Err(invalid(
            "boundary ports must be unique, contiguous and match wrapper",
        ));
    }
    let bounds = bounds.unwrap();
    let p = wrapper.position;
    if [p.left, p.top, p.right, p.bottom]
        .iter()
        .any(|x| !x.is_finite() || x.abs() > 1e9)
        || p.left >= p.right
        || p.top >= p.bottom
    {
        return Err(invalid("invalid wrapper geometry"));
    }
    let translation = Point {
        x: (p.left + p.right - bounds.left - bounds.right) / 2.,
        y: (p.top + p.bottom - bounds.top - bounds.bottom) / 2.,
    };
    for &i in &moved_indices {
        let p = child.blocks[i].position;
        if [
            p.left + translation.x,
            p.right + translation.x,
            p.top + translation.y,
            p.bottom + translation.y,
        ]
        .iter()
        .any(|x| !x.is_finite() || x.abs() > 1e9)
        {
            return Err(invalid("expanded positions exceed canvas bounds"));
        }
    }
    let pn = nets(parent, &parent_index)?;
    let cn = nets(child, &child_index)?;
    let mut grafts: BTreeMap<RootRef, Vec<Graft>> = BTreeMap::new();
    let mut consumed = BTreeSet::new();
    for (&number, b) in &inputs {
        let destination = ep(id, PortKind::In, number);
        let parent_root = *pn
            .drivers
            .get(&destination)
            .ok_or_else(|| invalid("unconnected wrapper input"))?;
        let child_root = *cn
            .sources
            .get(&ep(&b.id, PortKind::Out, 1))
            .ok_or_else(|| invalid("unconnected child input"))?;
        if parent.lines[parent_root].src.as_ref().unwrap().block == *id
            || cn.destinations[child_root]
                .iter()
                .any(|dst| child_index[&dst.block].block_type == "Outport")
        {
            return Err(invalid(
                "direct subsystem feedback or input/output pass-through is unsupported",
            ));
        }
        if named(&parent.lines[parent_root]) || named(&child.lines[child_root]) {
            return Err(invalid("named boundary nets cannot be expanded"));
        }
        grafts
            .entry(RootRef::Parent(parent_root))
            .or_default()
            .push(Graft {
                destination,
                donor: RootRef::Child(child_root),
            });
        consumed.insert(RootRef::Child(child_root));
    }
    for (&number, b) in &outputs {
        let destination = ep(&b.id, PortKind::In, 1);
        let child_root = *cn
            .drivers
            .get(&destination)
            .ok_or_else(|| invalid("unconnected child output"))?;
        let parent_root = *pn
            .sources
            .get(&ep(id, PortKind::Out, number))
            .ok_or_else(|| invalid("unconnected wrapper output"))?;
        if named(&parent.lines[parent_root]) || named(&child.lines[child_root]) {
            return Err(invalid("named boundary nets cannot be expanded"));
        }
        grafts
            .entry(RootRef::Child(child_root))
            .or_default()
            .push(Graft {
                destination,
                donor: RootRef::Parent(parent_root),
            });
        consumed.insert(RootRef::Parent(parent_root));
    }
    let mut lines = vec![];
    for base in (0..parent.lines.len())
        .map(RootRef::Parent)
        .chain((0..child.lines.len()).map(RootRef::Child))
    {
        if consumed.contains(&base) {
            continue;
        }
        let grafts = grafts.remove(&base).unwrap_or_default();
        let clear_points = !grafts.is_empty()
            || (matches!(base, RootRef::Child(_)) && (translation.x != 0. || translation.y != 0.));
        lines.push(ExpandedLineRecipe {
            base,
            grafts,
            clear_points,
        });
    }
    if !grafts.is_empty() {
        return Err(invalid("composed expansion bridges are unsupported"));
    }
    // A graft traverses its base tree and copies its donor. Bound cumulative
    // work, including repeated scans of a highly branched source root.
    fn nodes(line: &Line) -> usize {
        let mut count = 1;
        let mut stack: Vec<_> = line.branches.iter().collect();
        while let Some(b) = stack.pop() {
            count += 1;
            stack.extend(&b.branches);
        }
        count
    }
    let parent_nodes: Vec<_> = parent.lines.iter().map(nodes).collect();
    let child_nodes: Vec<_> = child.lines.iter().map(nodes).collect();
    let cost = |r: RootRef| match r {
        RootRef::Parent(i) => parent_nodes[i],
        RootRef::Child(i) => child_nodes[i],
    };
    let mut budget = 2_000_000usize;
    for recipe in &lines {
        let total = recipe
            .grafts
            .iter()
            .try_fold(cost(recipe.base), |n, g| n.checked_add(cost(g.donor)))
            .and_then(|n| n.checked_mul(recipe.grafts.len() + 1))
            .ok_or_else(|| invalid("expansion work budget exceeded"))?;
        budget = budget
            .checked_sub(total)
            .ok_or_else(|| invalid("expansion work budget exceeded"))?;
    }
    let mut next = next_sid(model).ok_or_else(|| invalid("no SIDs left"))?;
    let mut watermark = next - 1;
    let mut id_remap = vec![];
    for &i in &moved_indices {
        let old = &child.blocks[i].id;
        if old.0.parse::<u64>().is_err() {
            if model.source == SourceFormat::Slx {
                return Err(invalid("SLX expanded blocks require numeric SIDs"));
            }
            id_remap.push((old.clone(), BlockId(next.to_string())));
            watermark = next;
            next = next
                .checked_add(1)
                .ok_or_else(|| invalid("SID allocation overflows"))?;
        }
    }
    let mapped: BTreeMap<_, _> = id_remap.iter().cloned().collect();
    let moved_blocks = moved_indices
        .iter()
        .map(|&i| {
            let mut b = child.blocks[i].clone();
            if let Some(id) = mapped.get(&b.id) {
                b.id = id.clone();
            }
            b.position.left += translation.x;
            b.position.right += translation.x;
            b.position.top += translation.y;
            b.position.bottom += translation.y;
            b
        })
        .collect();
    Ok(ExpandPlan {
        wrapper_index: parent.blocks.iter().position(|b| b.id == *id).unwrap(),
        moved_indices,
        moved_blocks,
        child_names: child
            .blocks
            .iter()
            .map(|b| (b.id.clone(), b.name.clone()))
            .collect(),
        removed_port_ids: inputs
            .values()
            .chain(outputs.values())
            .map(|b| b.id.clone())
            .collect(),
        id_remap,
        translation,
        lines,
        watermark,
        parent_line_count: parent.lines.len(),
        child_line_count: child.lines.len(),
    })
}

/// Graft at the original destination node rather than flattening its ancestry.
/// Raw writers retain opaque properties on that same node and validate donor
/// root metadata before stripping the donor source.
pub fn materialize_lines(parent: &System, child: &System, plan: &ExpandPlan) -> Vec<Line> {
    fn get<'a>(parent: &'a System, child: &'a System, r: RootRef) -> &'a Line {
        match r {
            RootRef::Parent(i) => &parent.lines[i],
            RootRef::Child(i) => &child.lines[i],
        }
    }
    fn graft(
        dst: &mut Option<Endpoint>,
        branches: &mut Vec<Branch>,
        target: &Endpoint,
        donor: &Line,
    ) {
        if dst.as_ref() == Some(target) {
            *dst = None;
            if let Some(dst) = &donor.dst {
                branches.push(Branch {
                    dst: Some(dst.clone()),
                    ..Default::default()
                });
            }
            branches.extend(donor.branches.iter().cloned());
            return;
        }
        for b in branches {
            graft(&mut b.dst, &mut b.branches, target, donor);
        }
    }
    fn remap(ep: &mut Option<Endpoint>, map: &BTreeMap<BlockId, BlockId>) {
        if let Some(ep) = ep {
            if let Some(id) = map.get(&ep.block) {
                ep.block = id.clone();
            }
        }
    }
    fn finish(branches: &mut [Branch], clear: bool, map: &BTreeMap<BlockId, BlockId>) {
        for b in branches {
            remap(&mut b.dst, map);
            if clear {
                b.points.clear();
            }
            finish(&mut b.branches, clear, map);
        }
    }
    let map = plan.id_remap.iter().cloned().collect();
    plan.lines
        .iter()
        .map(|recipe| {
            let mut line = get(parent, child, recipe.base).clone();
            for g in &recipe.grafts {
                graft(
                    &mut line.dst,
                    &mut line.branches,
                    &g.destination,
                    get(parent, child, g.donor),
                );
            }
            remap(&mut line.src, &map);
            remap(&mut line.dst, &map);
            if recipe.clear_points {
                line.points.clear();
            }
            finish(&mut line.branches, recipe.clear_points, &map);
            line
        })
        .collect()
}

pub fn apply_expand(model: &mut Model, path: &SystemRef, id: &BlockId) -> Result<(), EditError> {
    let plan = plan_expand(model, path, id)?;
    let mut parent = &mut model.root;
    for ancestor in path {
        parent = parent
            .blocks
            .iter_mut()
            .find(|b| &b.id == ancestor)
            .unwrap()
            .subsystem
            .as_deref_mut()
            .unwrap();
    }
    let child = parent.blocks[plan.wrapper_index]
        .subsystem
        .as_deref()
        .unwrap();
    let lines = materialize_lines(parent, child, &plan);
    // Each original tree may fit individually while their grafted ancestry
    // exceeds the accepted serialization depth. Check before any mutation.
    for line in &lines {
        let mut pending: Vec<_> = line
            .branches
            .iter()
            .map(|branch| (branch, 1usize))
            .collect();
        while let Some((branch, depth)) = pending.pop() {
            if depth > 64 {
                return Err(invalid("expanded branch depth exceeds 64"));
            }
            pending.extend(branch.branches.iter().map(|branch| (branch, depth + 1)));
        }
    }
    parent
        .blocks
        .splice(plan.wrapper_index..=plan.wrapper_index, plan.moved_blocks);
    parent.lines = lines;
    model
        .root
        .properties
        .insert(SID_WATERMARK.into(), plan.watermark.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Edit;
    fn fixture() -> Model {
        let mut model = Model {
            name: "expand".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: Default::default(),
            root: Default::default(),
            workspace: Default::default(),
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
                name: format!("b{id}"),
                position: Rect::new(x, y, x + 30., y + 30.),
            }
            .apply(&mut model)
            .unwrap();
        }
        for (src, dst) in [("1", "2"), ("1", "5"), ("2", "3"), ("3", "4")] {
            Edit::Connect {
                system: vec![],
                src: ep(&src.into(), PortKind::Out, 1),
                dst: ep(&dst.into(), PortKind::In, 1),
            }
            .apply(&mut model)
            .unwrap();
        }
        model
    }
    fn group(model: &mut Model) {
        Edit::CreateSubsystem {
            system: vec![],
            ids: vec!["2".into(), "3".into()],
            id: "10".into(),
            name: "group".into(),
        }
        .apply(model)
        .unwrap();
    }
    fn expand(model: &mut Model) -> Result<(), EditError> {
        Edit::ExpandSubsystem {
            system: vec![],
            id: "10".into(),
        }
        .apply(model)
    }
    fn connections(system: &System) -> BTreeSet<(Endpoint, Endpoint)> {
        system
            .connections()
            .into_iter()
            .map(|c| (c.src, c.dst))
            .collect()
    }
    #[test]
    fn grouping_expansion_restores_connections_and_geometry() {
        let mut model = fixture();
        let before = model.clone();
        group(&mut model);
        let plan = plan_expand(&model, &vec![], &"10".into()).unwrap();
        assert_eq!(plan.translation, Point { x: 0., y: 0. });
        assert_eq!(plan.lines.iter().map(|l| l.grafts.len()).sum::<usize>(), 2);
        expand(&mut model).unwrap();
        assert_eq!(connections(&model.root), connections(&before.root));
        for block in &before.root.blocks {
            assert_eq!(model.root.block(&block.id), Some(block));
        }
        assert_eq!(next_sid(&model), Some(13));
        assert!(crate::validation::validate_structure(&model).is_valid());
    }
    #[test]
    fn moved_wrapper_translates_children_and_clears_internal_routes() {
        let mut model = fixture();
        model
            .root
            .lines
            .iter_mut()
            .find(|l| l.src.as_ref().unwrap().block.0 == "2")
            .unwrap()
            .points = vec![Point { x: 175., y: 15. }];
        group(&mut model);
        let wrapper = model
            .root
            .blocks
            .iter_mut()
            .find(|b| b.id.0 == "10")
            .unwrap();
        wrapper.position.left += 50.;
        wrapper.position.right += 50.;
        wrapper.position.top += 60.;
        wrapper.position.bottom += 60.;
        let plan = plan_expand(&model, &vec![], &"10".into()).unwrap();
        assert_eq!(plan.translation, Point { x: 50., y: 60. });
        assert!(plan
            .lines
            .iter()
            .filter(|l| matches!(l.base, RootRef::Child(_)))
            .all(|l| l.clear_points));
        expand(&mut model).unwrap();
        assert_eq!(
            model.root.block(&"2".into()).unwrap().position,
            Rect::new(150., 60., 180., 90.)
        );
        assert_eq!(
            model.root.block(&"1".into()).unwrap().position,
            Rect::new(0., 0., 30., 30.)
        );
    }
    #[test]
    fn graft_retains_receiver_and_donor_branch_ancestry() {
        let mut model = fixture();
        group(&mut model);
        let line = model
            .root
            .lines
            .iter_mut()
            .find(|l| l.src.as_ref().unwrap().block.0 == "1")
            .unwrap();
        let old = std::mem::take(&mut line.branches);
        line.branches = vec![Branch {
            branches: old,
            points: vec![Point { x: 3., y: 4. }],
            ..Default::default()
        }];
        expand(&mut model).unwrap();
        let line = model
            .root
            .lines
            .iter()
            .find(|l| l.src.as_ref().unwrap().block.0 == "1")
            .unwrap();
        assert_eq!(line.branches.len(), 1);
        assert!(line.branches[0].points.is_empty());
        assert!(line.branches[0]
            .branches
            .iter()
            .any(|b| !b.branches.is_empty()));
        assert_eq!(connections(&model.root), connections(&fixture().root));
    }
    #[test]
    fn numeric_sids_survive_and_legacy_children_receive_persistent_ids() {
        let mut model = fixture();
        group(&mut model);
        let child = model
            .root
            .blocks
            .iter_mut()
            .find(|b| b.id.0 == "10")
            .unwrap()
            .subsystem
            .as_deref_mut()
            .unwrap();
        child.blocks.iter_mut().find(|b| b.id.0 == "2").unwrap().id = "path:group/b2".into();
        fn remap(dst: &mut Option<Endpoint>, branches: &mut [Branch]) {
            if let Some(e) = dst {
                if e.block.0 == "2" {
                    e.block = "path:group/b2".into();
                }
            }
            for b in branches {
                remap(&mut b.dst, &mut b.branches);
            }
        }
        for l in &mut child.lines {
            remap(&mut l.src, &mut []);
            remap(&mut l.dst, &mut l.branches);
        }
        let plan = plan_expand(&model, &vec![], &"10".into()).unwrap();
        assert_eq!(plan.id_remap, vec![("path:group/b2".into(), "13".into())]);
        assert_eq!(plan.child_names[&BlockId::from("path:group/b2")], "b2");
        expand(&mut model).unwrap();
        assert!(model.root.block(&"13".into()).is_some());
        assert!(model.root.block(&"3".into()).is_some());
        assert_eq!(next_sid(&model), Some(14));
        assert!(crate::validation::validate_structure(&model).is_valid());
    }
    #[test]
    fn two_output_bridges_from_one_source_produce_one_root() {
        let mut model = fixture();
        group(&mut model);
        Edit::Disconnect {
            system: vec![],
            dst: ep(&"5".into(), PortKind::In, 1),
        }
        .apply(&mut model)
        .unwrap();
        let wrapper = model
            .root
            .blocks
            .iter_mut()
            .find(|b| b.id.0 == "10")
            .unwrap();
        wrapper.ports.outputs = 2;
        let child = wrapper.subsystem.as_deref_mut().unwrap();
        let mut out = child
            .blocks
            .iter()
            .find(|b| b.block_type == "Outport")
            .unwrap()
            .clone();
        out.id = "13".into();
        out.name = "Out2".into();
        out.parameters.insert("Port".into(), "2".into());
        child.blocks.push(out);
        let root = child
            .lines
            .iter_mut()
            .find(|l| l.src.as_ref().unwrap().block.0 == "3")
            .unwrap();
        root.branches.push(Branch {
            dst: Some(ep(&"13".into(), PortKind::In, 1)),
            ..Default::default()
        });
        model.root.lines.push(Line {
            src: Some(ep(&"10".into(), PortKind::Out, 2)),
            dst: Some(ep(&"5".into(), PortKind::In, 1)),
            ..Default::default()
        });
        expand(&mut model).unwrap();
        assert_eq!(
            model
                .root
                .lines
                .iter()
                .filter(|l| l.src.as_ref().unwrap().block.0 == "3")
                .count(),
            1
        );
        let links = connections(&model.root);
        for dst in ["4", "5"] {
            assert!(links.contains(&(
                ep(&"3".into(), PortKind::Out, 1),
                ep(&dst.into(), PortKind::In, 1)
            )));
        }
        assert!(crate::validation::validate_structure(&model).is_valid());
    }
    #[test]
    fn excessive_branch_depth_is_rejected_before_recursive_materialization() {
        let mut model = fixture();
        group(&mut model);
        let root = &mut model.root.lines[0];
        let mut branch = Branch {
            dst: root.dst.take(),
            branches: std::mem::take(&mut root.branches),
            ..Default::default()
        };
        for _ in 0..65 {
            branch = Branch {
                branches: vec![branch],
                ..Default::default()
            };
        }
        root.branches = vec![branch];
        let before = model.clone();
        assert!(expand(&mut model)
            .unwrap_err()
            .to_string()
            .contains("depth"));
        assert_eq!(model, before);
    }
    #[test]
    fn combined_graft_depth_is_checked_before_mutation() {
        fn deepen(line: &mut Line) {
            let mut branch = Branch {
                dst: line.dst.take(),
                branches: std::mem::take(&mut line.branches),
                ..Default::default()
            };
            for _ in 0..39 {
                branch = Branch {
                    branches: vec![branch],
                    ..Default::default()
                };
            }
            line.branches = vec![branch];
        }
        let mut model = fixture();
        group(&mut model);
        deepen(
            model
                .root
                .lines
                .iter_mut()
                .find(|l| l.src.as_ref().unwrap().block.0 == "1")
                .unwrap(),
        );
        let child = model
            .root
            .blocks
            .iter_mut()
            .find(|b| b.id.0 == "10")
            .unwrap()
            .subsystem
            .as_deref_mut()
            .unwrap();
        let input = child
            .blocks
            .iter()
            .find(|b| b.block_type == "Inport")
            .unwrap()
            .id
            .clone();
        deepen(
            child
                .lines
                .iter_mut()
                .find(|l| l.src.as_ref().unwrap().block == input)
                .unwrap(),
        );
        // Both original trees fit. Only the composed ancestry exceeds the cap.
        assert!(plan_expand(&model, &vec![], &"10".into()).is_ok());
        let before = model.clone();
        assert!(expand(&mut model)
            .unwrap_err()
            .to_string()
            .contains("expanded branch depth"));
        assert_eq!(model, before);
    }
    #[test]
    fn unsafe_expansions_leave_the_model_unchanged() {
        let mut original = fixture();
        group(&mut original);
        for case in 0..8 {
            let mut model = original.clone();
            let wrapper = model
                .root
                .blocks
                .iter_mut()
                .find(|b| b.id.0 == "10")
                .unwrap();
            match case {
                0 => {
                    wrapper
                        .parameters
                        .insert("TreatAsAtomicUnit".into(), "on".into());
                }
                1 => {
                    wrapper
                        .subsystem
                        .as_deref_mut()
                        .unwrap()
                        .properties
                        .insert("UserData".into(), "opaque".into());
                }
                2 => {
                    wrapper.subsystem.as_deref_mut().unwrap().blocks[0].name = "b1".into();
                }
                3 => {
                    let port = wrapper
                        .subsystem
                        .as_deref_mut()
                        .unwrap()
                        .blocks
                        .iter_mut()
                        .find(|b| b.block_type == "Inport")
                        .unwrap();
                    port.parameters.insert("SampleTime".into(), "0.1".into());
                }
                4 => {
                    wrapper.mirrored = true;
                }
                5 => {
                    wrapper.subsystem.as_deref_mut().unwrap().lines[0].name =
                        Some("boundary".into());
                    model.root.lines[0].name = Some("boundary".into());
                }
                6 => {
                    model.root.lines.push(model.root.lines[0].clone());
                }
                _ => {
                    wrapper.subsystem.as_deref_mut().unwrap().blocks[0]
                        .parameters
                        .insert("InitFcn".into(), "x=1".into());
                }
            }
            let before = model.clone();
            assert!(expand(&mut model).is_err(), "case {case}");
            assert_eq!(before, model);
        }
    }
}
