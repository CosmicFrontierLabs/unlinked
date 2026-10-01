//! Conversion from the format-neutral [`Node`] tree to the model IR.

use crate::tree::Node;
use crate::ImportError;
use std::collections::{BTreeMap, HashMap};
use unlinked_model::geometry::{from_rotation, port_anchor};
use unlinked_model::{
    Annotation, Block, BlockId, BlockStyle, Branch, Endpoint, Line, Mask, MaskParameter,
    NamePlacement, Orientation, Point, PortCounts, PortInterface, PortKind, PortRef, Rect,
    SimConfig, System,
};

/// Block properties that map onto dedicated IR fields rather than being
/// copied into `Block::parameters`.
const CONSUMED_BLOCK_KEYS: &[&str] = &[
    "BlockType",
    "Name",
    "SID",
    "Position",
    "Ports",
    "Orientation",
    "BlockRotation",
    "BlockMirror",
    "ForegroundColor",
    "BackgroundColor",
    "ShowName",
    "NamePlacement",
    "DropShadow",
    "FontSize",
    "SourceBlock",
];

/// Largest port index or per-kind port count accepted from a file. Larger
/// values are clamped (counts) or the referencing line end dropped (indices).
const MAX_PORTS: u32 = 1024;

/// `<PortCounts>` attribute names, in `Ports` vector order.
const PORT_COUNT_ATTRS: [&str; 9] = [
    "in", "out", "enable", "trigger", "state", "lconn", "rconn", "ifaction", "reset",
];

/// Per-block-type parameter defaults (`BlockParameterDefaults`), applied to
/// every block for keys the block does not set itself.
pub type TypeDefaults = HashMap<String, Vec<(String, String)>>;

pub fn read_type_defaults(node: &Node, out: &mut TypeDefaults) {
    if let Some(defaults) = node.find(&|n| n.tag == "BlockParameterDefaults") {
        for block in defaults.children_named("Block") {
            if let Some(ty) = block.get("BlockType") {
                let entry = out.entry(ty.to_string()).or_default();
                for (k, v) in &block.props {
                    if k != "BlockType" {
                        entry.push((k.clone(), v.clone()));
                    }
                }
            }
        }
    }
}

pub struct Converter<'a> {
    pub defaults: &'a TypeDefaults,
    /// Counter for blocks with no SID.
    next_synthetic: usize,
}

impl<'a> Converter<'a> {
    pub fn new(defaults: &'a TypeDefaults) -> Self {
        Converter {
            defaults,
            next_synthetic: 0,
        }
    }

    pub fn system(&mut self, node: &Node, path: &str) -> Result<System, ImportError> {
        let mut blocks = Vec::new();
        for b in node.children_named("Block") {
            blocks.push(self.block(b, path)?);
        }

        let names: HashMap<&str, BlockId> = blocks
            .iter()
            .map(|b| (b.name.as_str(), b.id.clone()))
            .collect();
        let raw_lines: Vec<RawLine> = node
            .children_named("Line")
            .map(|l| raw_line(l, &names))
            .collect();

        bump_port_counts(&mut blocks, &raw_lines);

        let by_id: HashMap<&BlockId, &Block> = blocks.iter().map(|b| (&b.id, b)).collect();
        let mut lines = Vec::new();
        for l in &raw_lines {
            resolve_line(l, &by_id, &mut lines);
        }

        let annotations = node.children_named("Annotation").map(annotation).collect();

        let properties = node
            .props
            .iter()
            .filter(|(k, _)| k != "Name")
            .cloned()
            .collect();

        Ok(System {
            blocks,
            lines,
            annotations,
            properties,
        })
    }

    fn block(&mut self, node: &Node, parent_path: &str) -> Result<Block, ImportError> {
        let block_type = node.get("BlockType").unwrap_or("Unknown").to_string();
        let name = node.get("Name").unwrap_or_default().to_string();
        let path = format!("{parent_path}/{}", unlinked_model::escape_name(&name));
        let id = match node.get("SID") {
            Some(sid) if !sid.is_empty() => BlockId(sid.to_string()),
            _ => {
                self.next_synthetic += 1;
                BlockId(format!("path:{path}"))
            }
        };

        let position = node
            .get("Position")
            .and_then(|p| {
                let v = parse_numbers(p);
                (v.len() >= 4).then(|| Rect::new(v[0], v[1], v[2], v[3]))
            })
            .unwrap_or_default();

        let (orientation, mirrored) = orientation(node);

        let clamp = |x: f64| x.clamp(0.0, MAX_PORTS as f64) as u32;
        let ports = if let Some(p) = node.get("Ports") {
            let v: Vec<u32> = parse_numbers(p).into_iter().map(clamp).collect();
            PortCounts::from_slice(&v)
        } else if let Some(pc) = node.child("PortCounts") {
            // R2024b+: <PortCounts in="2" out="1" lconn="1" .../>
            let v: Vec<u32> = PORT_COUNT_ATTRS
                .iter()
                .map(|k| {
                    pc.attr(k)
                        .and_then(|s| s.trim().parse().ok())
                        .map_or(0, clamp)
                })
                .collect();
            PortCounts::from_slice(&v)
        } else {
            default_ports(&block_type, node)
        };

        let mut parameters = BTreeMap::new();
        for (k, v) in &node.props {
            if !CONSUMED_BLOCK_KEYS.contains(&k.as_str()) {
                parameters.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &node.attrs {
            if !CONSUMED_BLOCK_KEYS.contains(&k.as_str()) {
                parameters.insert(k.clone(), v.clone());
            }
        }
        if let Some(inst) = node.child("InstanceData") {
            for (k, v) in &inst.props {
                parameters.insert(k.clone(), v.clone());
            }
        }
        // Bus element ports describe their interface in a list.
        let interface = node
            .children_named("List")
            .find(|l| l.get("ListType") == Some("InterfaceData"))
            .map(|l| PortInterface::from_properties(l.props.iter().cloned().collect()));
        if let Some(defaults) = self.defaults.get(&block_type) {
            for (k, v) in defaults {
                if !CONSUMED_BLOCK_KEYS.contains(&k.as_str()) {
                    parameters.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }

        let subsystem = match node.child("System") {
            Some(sys) => Some(Box::new(self.system(sys, &path)?)),
            None => None,
        };

        let style = BlockStyle {
            foreground: node.get("ForegroundColor").map(str::to_string),
            background: node.get("BackgroundColor").map(str::to_string),
            show_name: node.get("ShowName") != Some("off"),
            name_placement: match node.get("NamePlacement") {
                Some("alternate") => NamePlacement::Alternate,
                _ => NamePlacement::Normal,
            },
            drop_shadow: node.get("DropShadow") == Some("on"),
            font_size: node.get("FontSize").and_then(|s| s.trim().parse().ok()),
        };

        Ok(Block {
            id,
            block_type,
            name,
            position,
            orientation,
            mirrored,
            ports,
            parameters,
            mask: mask(node),
            library_source: node.get("SourceBlock").map(str::to_string),
            subsystem,
            style,
            interface,
        })
    }
}

/// Map `Orientation` (MDL) or `BlockRotation` + `BlockMirror` (newer files)
/// onto flow direction plus a flag for control ports on the non-default side.
fn orientation(node: &Node) -> (Orientation, bool) {
    let mirror = node.get("BlockMirror") == Some("on");
    if let Some(rot) = node.get("BlockRotation") {
        return from_rotation(rot.trim().parse::<i32>().unwrap_or(0), mirror);
    }
    let o = match node.get("Orientation") {
        Some("left") => Orientation::Left,
        Some("up") => Orientation::Up,
        Some("down") => Orientation::Down,
        _ if mirror => Orientation::Left,
        _ => Orientation::Right,
    };
    (o, false)
}

const SOURCE_TYPES: &[&str] = &[
    "Inport",
    "Constant",
    "Ground",
    "Clock",
    "DigitalClock",
    "Step",
    "Sin",
    "SignalGenerator",
    "RandomNumber",
    "UniformRandomNumber",
    "DiscretePulseGenerator",
    "FromWorkspace",
    "FromFile",
    "From",
    "DataStoreRead",
    "Ramp",
];

const SINK_TYPES: &[&str] = &[
    "Outport",
    "Terminator",
    "Display",
    "ToWorkspace",
    "ToFile",
    "Goto",
    "DataStoreWrite",
    "Stop",
];

const PORTLESS_TYPES: &[&str] = &[
    "DataStoreMemory",
    "SubSystem",
    "Reference",
    "GotoTagVisibility",
    "ModelReference",
];

/// Port counts for blocks that omit `Ports` (the type's default was used).
/// Counts implied by connected lines are merged in later.
fn default_ports(block_type: &str, node: &Node) -> PortCounts {
    if block_type == "Scope" {
        let n = node
            .get("NumInputPorts")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        return PortCounts::from_slice(&[n, 0]);
    }
    if SOURCE_TYPES.contains(&block_type) {
        PortCounts::from_slice(&[0, 1])
    } else if SINK_TYPES.contains(&block_type) {
        PortCounts::from_slice(&[1, 0])
    } else if PORTLESS_TYPES.contains(&block_type) {
        PortCounts::default()
    } else {
        PortCounts::from_slice(&[1, 1])
    }
}

fn mask(node: &Node) -> Option<Mask> {
    let text_of = |n: &Node, key: &str| -> Option<String> {
        n.get(key)
            .map(str::to_string)
            .or_else(|| n.child(key).map(|c| c.text.clone()))
    };

    let mask_node = node.children.iter().find(|c| {
        c.tag == "Mask"
            || c.get("PropName") == Some("MaskObject")
            || c.get("$PropName") == Some("MaskObject")
    });

    if let Some(m) = mask_node {
        let mut parameters = Vec::new();
        collect_mask_params(m, false, &mut parameters, &text_of);
        return Some(Mask {
            mask_type: text_of(m, "Type"),
            description: text_of(m, "Description"),
            display: text_of(m, "Display"),
            parameters,
        });
    }

    // Pre-R2012 style: flat Mask* properties on the block.
    let old_keys = [
        "MaskType",
        "MaskDisplay",
        "MaskVariables",
        "MaskPromptString",
    ];
    if !old_keys.iter().any(|k| node.get(k).is_some()) {
        return None;
    }
    let names: Vec<String> = node
        .get("MaskVariables")
        .unwrap_or_default()
        .split(';')
        .filter_map(|v| v.split('=').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    let prompts: Vec<&str> = node
        .get("MaskPromptString")
        .unwrap_or_default()
        .split('|')
        .collect();
    let values: Vec<&str> = node
        .get("MaskValueString")
        .unwrap_or_default()
        .split('|')
        .collect();
    let styles: Vec<&str> = node
        .get("MaskStyleString")
        .unwrap_or_default()
        .split(',')
        .collect();
    let parameters = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| MaskParameter {
            name,
            prompt: prompts.get(i).map(|s| s.to_string()),
            value: values.get(i).map(|s| s.to_string()).unwrap_or_default(),
            kind: styles
                .get(i)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty()),
        })
        .collect();
    Some(Mask {
        mask_type: node.get("MaskType").map(str::to_string),
        description: node.get("MaskDescription").map(str::to_string),
        display: node.get("MaskDisplay").map(str::to_string),
        parameters,
    })
}

fn collect_mask_params(
    node: &Node,
    in_param_array: bool,
    out: &mut Vec<MaskParameter>,
    text_of: &dyn Fn(&Node, &str) -> Option<String>,
) {
    for c in &node.children {
        let class = c.get("ClassName").or_else(|| c.get("$ClassName"));
        let is_param = c.tag == "MaskParameter"
            || class == Some("Simulink.MaskParameter")
            || (in_param_array && c.tag == "Object");
        if is_param {
            if let Some(name) = text_of(c, "Name") {
                out.push(MaskParameter {
                    name,
                    prompt: text_of(c, "Prompt"),
                    value: text_of(c, "Value").unwrap_or_default(),
                    kind: text_of(c, "Type"),
                });
            }
            continue;
        }
        let param_array = c.tag == "Array" && c.get("Type") == Some("Simulink.MaskParameter");
        collect_mask_params(c, param_array, out, text_of);
    }
}

/// A line with endpoints resolved to block ids but vertices still relative.
struct RawLine {
    name: Option<String>,
    src: Option<Endpoint>,
    points: Vec<Point>,
    dst: Option<Endpoint>,
    branches: Vec<RawBranch>,
}

struct RawBranch {
    points: Vec<Point>,
    dst: Option<Endpoint>,
    branches: Vec<RawBranch>,
    /// A `DEST_DEST` junction with no port of its own: the start of a
    /// separately drawn sub-net whose first vertex is absolute.
    detached: bool,
}

fn raw_line(node: &Node, names: &HashMap<&str, BlockId>) -> RawLine {
    RawLine {
        name: node.get("Name").map(str::to_string),
        src: endpoint(node, "Src", "SrcBlock", "SrcPort", PortKind::Out, names),
        points: parse_points(node.get("Points")),
        dst: endpoint(node, "Dst", "DstBlock", "DstPort", PortKind::In, names),
        branches: node
            .children_named("Branch")
            .map(|b| raw_branch(b, names))
            .collect(),
    }
}

/// Branch endpoints are normally `Dst`; physical connection trees (Simscape)
/// have no line source and store each branch's port as `Src` instead.
fn raw_branch(node: &Node, names: &HashMap<&str, BlockId>) -> RawBranch {
    let dst = endpoint(node, "Dst", "DstBlock", "DstPort", PortKind::In, names)
        .or_else(|| endpoint(node, "Src", "SrcBlock", "SrcPort", PortKind::Out, names));
    RawBranch {
        points: parse_points(node.get("Points")),
        detached: dst.is_none() && node.attr("ConnectType") == Some("DEST_DEST"),
        dst,
        branches: node
            .children_named("Branch")
            .map(|b| raw_branch(b, names))
            .collect(),
    }
}

/// Resolve an endpoint written either as `Src "12#out:1"` (SLX and newer MDL)
/// or as `SrcBlock "name"` + `SrcPort 1` (older MDL).
fn endpoint(
    node: &Node,
    sid_key: &str,
    block_key: &str,
    port_key: &str,
    default_kind: PortKind,
    names: &HashMap<&str, BlockId>,
) -> Option<Endpoint> {
    if let Some(s) = node.get(sid_key) {
        let (block, port) = s.split_once('#')?;
        return Some(Endpoint {
            block: BlockId(block.to_string()),
            port: parse_port(port, default_kind)?,
        });
    }
    let block = names.get(node.get(block_key)?)?.clone();
    let port = parse_port(node.get(port_key).unwrap_or("1"), default_kind)?;
    Some(Endpoint { block, port })
}

/// Parse `out:1`, `in:2`, `state`, `enable`, `LConn1`, or a bare number.
/// Indices outside `1..=MAX_PORTS` are rejected.
pub(crate) fn parse_port(s: &str, default_kind: PortKind) -> Option<PortRef> {
    let s = s.trim();
    let (kind, index) = if let Ok(index) = s.parse::<u32>() {
        (default_kind, index)
    } else {
        let (kind, index) = match s.split_once(':') {
            Some((k, i)) => (k, i.trim().parse().ok()?),
            None => {
                let digits = s.trim_start_matches(|c: char| !c.is_ascii_digit());
                let kind = &s[..s.len() - digits.len()];
                (
                    kind,
                    if digits.is_empty() {
                        1
                    } else {
                        digits.parse().ok()?
                    },
                )
            }
        };
        (PortKind::from_token(kind)?, index)
    };
    (1..=MAX_PORTS)
        .contains(&index)
        .then_some(PortRef { kind, index })
}

/// Grow each block's port counts to cover every port a line references.
fn bump_port_counts(blocks: &mut [Block], lines: &[RawLine]) {
    let mut refs: Vec<&Endpoint> = Vec::new();
    fn branch_refs<'a>(bs: &'a [RawBranch], out: &mut Vec<&'a Endpoint>) {
        for b in bs {
            out.extend(b.dst.as_ref());
            branch_refs(&b.branches, out);
        }
    }
    for l in lines {
        refs.extend(l.src.as_ref());
        refs.extend(l.dst.as_ref());
        branch_refs(&l.branches, &mut refs);
    }
    for block in blocks.iter_mut() {
        for ep in refs.iter().filter(|e| e.block == block.id) {
            let p = &mut block.ports;
            let slot = match ep.port.kind {
                PortKind::In => &mut p.inputs,
                PortKind::Out => &mut p.outputs,
                PortKind::Enable => &mut p.enable,
                PortKind::Trigger => &mut p.trigger,
                PortKind::State => &mut p.state,
                PortKind::LConn => &mut p.lconn,
                PortKind::RConn => &mut p.rconn,
                PortKind::IfAction => &mut p.ifaction,
                PortKind::Reset => &mut p.reset,
            };
            *slot = (*slot).max(ep.port.index);
        }
    }
}

fn anchor(ep: &Option<Endpoint>, blocks: &HashMap<&BlockId, &Block>) -> Option<Point> {
    let ep = ep.as_ref()?;
    Some(port_anchor(blocks.get(&ep.block)?, ep.port))
}

/// Turn relative vertices into absolute ones. The first vertex is relative
/// to `start` when known; without a start the first vertex is absolute.
fn accumulate(start: Option<Point>, rel: &[Point]) -> Vec<Point> {
    let mut out = Vec::with_capacity(rel.len());
    let mut cur = start;
    for p in rel {
        let next = match cur {
            Some(c) => Point::new(c.x + p.x, c.y + p.y),
            None => *p,
        };
        out.push(next);
        cur = Some(next);
    }
    out
}

/// Resolve a line to absolute coordinates. Detached sub-nets are split off
/// into their own source-less lines, appended to `out` after this one.
fn resolve_line(line: &RawLine, blocks: &HashMap<&BlockId, &Block>, out: &mut Vec<Line>) {
    let start = anchor(&line.src, blocks);
    let points = accumulate(start, &line.points);
    let tail = points.last().copied().or(start);
    let mut detached = Vec::new();
    let branches = resolve_branches(&line.branches, tail, &mut detached);
    out.push(Line {
        name: line.name.clone(),
        src: line.src.clone(),
        points,
        dst: line.dst.clone(),
        branches,
    });
    for d in detached {
        push_detached(d, out);
    }
}

fn push_detached(d: &RawBranch, out: &mut Vec<Line>) {
    let points = accumulate(None, &d.points);
    let tail = points.last().copied();
    let mut more = Vec::new();
    let branches = resolve_branches(&d.branches, tail, &mut more);
    out.push(Line {
        name: None,
        src: None,
        points,
        dst: None,
        branches,
    });
    for m in more {
        push_detached(m, out);
    }
}

fn resolve_branches<'r>(
    bs: &'r [RawBranch],
    start: Option<Point>,
    detached: &mut Vec<&'r RawBranch>,
) -> Vec<Branch> {
    let mut out = Vec::new();
    for b in bs {
        if b.detached {
            detached.push(b);
            continue;
        }
        let points = accumulate(start, &b.points);
        let tail = points.last().copied().or(start);
        out.push(Branch {
            points,
            dst: b.dst.clone(),
            branches: resolve_branches(&b.branches, tail, detached),
        });
    }
    out
}

fn annotation(node: &Node) -> Annotation {
    let v = node.get("Position").map(parse_numbers).unwrap_or_default();
    let position = match v.len() {
        n if n >= 4 => Rect::new(v[0], v[1], v[2], v[3]),
        2 | 3 => Rect::new(v[0], v[1], v[0], v[1]),
        _ => Rect::default(),
    };
    let text = node
        .get("Text")
        .or_else(|| node.get("Name"))
        .map(str::to_string)
        .unwrap_or_else(|| node.text.trim().to_string());
    let rich_text = node.get("Interpreter") == Some("rich");
    let properties = node
        .props
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "Position" | "Text" | "Name"))
        .cloned()
        .collect();
    Annotation {
        text,
        position,
        rich_text,
        properties,
    }
}

/// Collect solver settings from the `Simulink.SolverCC` component of the
/// configuration set, falling back to model-level properties (old MDL).
pub fn sim_config(trees: &[&Node], model: &Node) -> SimConfig {
    let is_solver =
        |n: &Node| n.tag == "Simulink.SolverCC" || n.get("ClassName") == Some("Simulink.SolverCC");
    let mut raw = BTreeMap::new();
    if let Some(solver) = trees.iter().find_map(|t| t.find(&is_solver)) {
        for (k, v) in &solver.props {
            if !k.starts_with('$') {
                raw.insert(k.clone(), v.clone());
            }
        }
    }
    for key in ["Solver", "StartTime", "StopTime", "FixedStep", "SolverName"] {
        if let Some(v) = model.prop(key) {
            raw.entry(key.to_string()).or_insert_with(|| v.to_string());
        }
    }
    SimConfig {
        solver: raw.get("Solver").or_else(|| raw.get("SolverName")).cloned(),
        start_time: raw.get("StartTime").cloned(),
        stop_time: raw.get("StopTime").cloned(),
        fixed_step: raw.get("FixedStep").cloned(),
        raw,
    }
}

/// Parse a MATLAB numeric vector/matrix literal like `[1, 2; 3, 4]` into a
/// flat list. Non-numeric entries are skipped.
pub fn parse_numbers(s: &str) -> Vec<f64> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect()
}

fn parse_points(s: Option<&str>) -> Vec<Point> {
    let Some(s) = s else { return Vec::new() };
    let v = parse_numbers(s);
    v.as_chunks::<2>()
        .0
        .iter()
        .map(|[x, y]| Point::new(*x, *y))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_parse() {
        let p = |s| parse_port(s, PortKind::In).unwrap();
        assert_eq!(
            p("out:2"),
            PortRef {
                kind: PortKind::Out,
                index: 2
            }
        );
        assert_eq!(
            p("state"),
            PortRef {
                kind: PortKind::State,
                index: 1
            }
        );
        assert_eq!(
            p("3"),
            PortRef {
                kind: PortKind::In,
                index: 3
            }
        );
        assert_eq!(
            p("enable"),
            PortRef {
                kind: PortKind::Enable,
                index: 1
            }
        );
        assert_eq!(
            p("LConn2"),
            PortRef {
                kind: PortKind::LConn,
                index: 2
            }
        );
        assert_eq!(
            p("Reset"),
            PortRef {
                kind: PortKind::Reset,
                index: 1
            }
        );
        assert!(parse_port("bogus", PortKind::In).is_none());
        assert!(parse_port("0", PortKind::In).is_none());
        assert!(parse_port("4294967295", PortKind::Out).is_none());
        assert!(parse_port("out:99999", PortKind::Out).is_none());
    }

    #[test]
    fn numbers_parse() {
        assert_eq!(
            parse_numbers("[53, 0; 0, -105]"),
            vec![53.0, 0.0, 0.0, -105.0]
        );
        assert_eq!(parse_numbers("[]"), Vec::<f64>::new());
    }

    #[test]
    fn rotation_mapping() {
        let mut n = Node::new("Block");
        n.props.push(("BlockRotation".into(), "270".into()));
        assert_eq!(orientation(&n), (Orientation::Up, false));
        n.props.push(("BlockMirror".into(), "on".into()));
        assert_eq!(orientation(&n), (Orientation::Down, true));
        let mut m = Node::new("Block");
        m.props.push(("BlockMirror".into(), "on".into()));
        assert_eq!(orientation(&m), (Orientation::Left, false));
    }
}
