//! Model intermediate representation shared by import, rendering and
//! simulation.
//!
//! The IR mirrors Simulink's own concepts closely: a [`Model`] owns a root
//! [`System`] (one diagram level), which holds [`Block`]s connected by
//! [`Line`]s. Subsystems nest a further `System` inside their block.
//! Block parameters are kept as raw MATLAB expression strings; evaluating them
//! is left to consumers that need numbers.

pub mod boundary;
pub mod catalog;
pub mod clipboard;
pub mod config;
pub mod diff;
pub mod duplicate;
pub mod edit;
pub mod expand;
pub mod geometry;
pub mod hierarchy;
pub mod route_edit;
pub mod scope;
pub mod stateflow;
pub mod validation;

pub use stateflow::{
    Chart, ChartData, ChartKind, DataScope, Junction, JunctionKind, State, StateKind, Transition,
};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A complete block diagram loaded from a `.slx` or `.mdl` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    pub source: SourceFormat,
    /// Simulink release that saved the file, e.g. `"23.2"` or `"R2012b"`.
    pub simulink_version: Option<String>,
    pub config: SimConfig,
    pub root: System,
    /// Model workspace variables as `name -> MATLAB expression`.
    pub workspace: BTreeMap<String, String>,
    /// Stateflow charts and MATLAB Function blocks.
    #[serde(default)]
    pub charts: Vec<Chart>,
    /// Document parameter defaults, excluding properties represented by dedicated block fields.
    #[serde(default)]
    pub type_defaults: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFormat {
    Slx,
    Mdl,
}

/// Solver configuration. Values are raw MATLAB expressions as stored in the
/// file; `raw` keeps every configuration parameter that was read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimConfig {
    pub solver: Option<String>,
    pub start_time: Option<String>,
    pub stop_time: Option<String>,
    pub fixed_step: Option<String>,
    pub raw: BTreeMap<String, String>,
}

/// One level of a block diagram.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct System {
    pub blocks: Vec<Block>,
    pub lines: Vec<Line>,
    pub annotations: Vec<Annotation>,
    /// Remaining system-level properties (`ZoomFactor`, `Location`, ...).
    pub properties: BTreeMap<String, String>,
}

/// Identifies a block within a model. Uses the Simulink `SID` when the file
/// has one, otherwise an id synthesized from the block path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockId(pub String);

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BlockId {
    fn from(s: &str) -> Self {
        BlockId(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    /// Simulink `BlockType`, e.g. `Gain`, `Sum`, `SubSystem`, `Reference`.
    pub block_type: String,
    pub name: String,
    pub position: Rect,
    pub orientation: Orientation,
    pub mirrored: bool,
    pub ports: PortCounts,
    /// Every other block parameter as `name -> raw value`.
    pub parameters: BTreeMap<String, String>,
    pub mask: Option<Mask>,
    /// Library path for linked blocks (`SourceBlock`), e.g.
    /// `simulink/Discontinuities/Saturation`.
    pub library_source: Option<String>,
    pub subsystem: Option<Box<System>>,
    pub style: BlockStyle,
    /// Bus element port interface (`InterfaceData`), for In/Out Bus
    /// Element blocks. Several such blocks can share one port number.
    #[serde(default)]
    pub interface: Option<PortInterface>,
}

/// The interface of a bus element port, as the file records it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PortInterface {
    /// Port of the parent subsystem this block belongs to.
    pub port_number: Option<u32>,
    pub port_name: Option<String>,
    /// Bus element path within the port, e.g. `motorsensors.Iabc`.
    pub element: Option<String>,
    pub is_composite: Option<bool>,
    pub is_client_server: Option<bool>,
    /// Every property as written, including the ones above.
    pub raw: BTreeMap<String, String>,
}

impl PortInterface {
    /// Read the properties of an `InterfaceData` list. Unparsable values
    /// stay `None` here and remain available in `raw`.
    pub fn from_properties(raw: BTreeMap<String, String>) -> Self {
        let text = |k: &str| raw.get(k).filter(|v| !v.is_empty()).cloned();
        let flag = |k: &str| match raw.get(k).map(|v| v.trim()) {
            Some("1" | "on" | "true") => Some(true),
            Some("0" | "off" | "false") => Some(false),
            _ => None,
        };
        PortInterface {
            port_number: raw.get("PortNumber").and_then(|v| v.trim().parse().ok()),
            port_name: text("PortName"),
            element: text("Element"),
            is_composite: flag("IsComposite"),
            is_client_server: flag("IsClientServer"),
            raw,
        }
    }
}

impl Block {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.parameters.get(name).map(String::as_str)
    }

    /// Stateflow block kind (`SFBlockType`, e.g. `Chart` or `MATLAB
    /// Function`) for blocks backed by a Stateflow chart.
    pub fn stateflow_type(&self) -> Option<&str> {
        self.param("SFBlockType")
            .filter(|t| !t.is_empty() && *t != "NONE")
    }

    /// Mask type when masked, else the library source's final path element,
    /// else the block type. This is the best "what is this block" label.
    pub fn display_type(&self) -> &str {
        if let Some(t) = self.mask.as_ref().and_then(|m| m.mask_type.as_deref()) {
            if !t.is_empty() {
                return t;
            }
        }
        if let Some(src) = &self.library_source {
            if let Some(last) = src.rsplit('/').next() {
                return last;
            }
        }
        &self.block_type
    }
}

/// Axis-aligned rectangle in Simulink canvas coordinates (y grows downward).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }

    pub fn center(&self) -> Point {
        Point::new(
            (self.left + self.right) / 2.0,
            (self.top + self.bottom) / 2.0,
        )
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    pub fn include_point(&self, p: Point) -> Rect {
        self.union(&Rect::new(p.x, p.y, p.x, p.y))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

/// Direction the block's signal flow faces (`Orientation` in the file).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    #[default]
    Right,
    Left,
    Up,
    Down,
}

/// Number of ports of each kind, in the order of Simulink's `Ports` vector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortCounts {
    pub inputs: u32,
    pub outputs: u32,
    pub enable: u32,
    pub trigger: u32,
    pub state: u32,
    pub lconn: u32,
    pub rconn: u32,
    pub ifaction: u32,
    pub reset: u32,
}

impl PortCounts {
    /// Build from Simulink's `Ports` vector `[in, out, enable, trigger,
    /// state, lconn, rconn, ifaction, reset]`; missing entries are zero.
    pub fn from_slice(v: &[u32]) -> Self {
        let g = |i: usize| v.get(i).copied().unwrap_or(0);
        PortCounts {
            inputs: g(0),
            outputs: g(1),
            enable: g(2),
            trigger: g(3),
            state: g(4),
            lconn: g(5),
            rconn: g(6),
            ifaction: g(7),
            reset: g(8),
        }
    }

    pub fn count(&self, kind: PortKind) -> u32 {
        match kind {
            PortKind::In => self.inputs,
            PortKind::Out => self.outputs,
            PortKind::Enable => self.enable,
            PortKind::Trigger => self.trigger,
            PortKind::State => self.state,
            PortKind::LConn => self.lconn,
            PortKind::RConn => self.rconn,
            PortKind::IfAction => self.ifaction,
            PortKind::Reset => self.reset,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PortKind {
    In,
    Out,
    Enable,
    Trigger,
    State,
    LConn,
    RConn,
    IfAction,
    Reset,
}

impl PortKind {
    /// Parse the port kind token used in SLX endpoints (`5#out:1`) and MDL
    /// `DstPort` values (`enable`, `trigger`, ...).
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "in" => PortKind::In,
            "out" => PortKind::Out,
            "enable" => PortKind::Enable,
            "trigger" => PortKind::Trigger,
            "state" => PortKind::State,
            "lconn" => PortKind::LConn,
            "rconn" => PortKind::RConn,
            "ifaction" => PortKind::IfAction,
            "reset" => PortKind::Reset,
            _ => return None,
        })
    }

    pub fn token(&self) -> &'static str {
        match self {
            PortKind::In => "in",
            PortKind::Out => "out",
            PortKind::Enable => "enable",
            PortKind::Trigger => "trigger",
            PortKind::State => "state",
            PortKind::LConn => "lconn",
            PortKind::RConn => "rconn",
            PortKind::IfAction => "ifaction",
            PortKind::Reset => "reset",
        }
    }
}

/// A port on a block. `index` is 1-based, as in Simulink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortRef {
    pub kind: PortKind,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub block: BlockId,
    pub port: PortRef,
}

/// The SLX form: `12#out:1`, or `12#enable` for the single control ports.
impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.block, self.port.kind.token())?;
        match self.port.kind {
            PortKind::In | PortKind::Out | PortKind::LConn | PortKind::RConn => {
                write!(f, ":{}", self.port.index)
            }
            _ => Ok(()),
        }
    }
}

/// A signal line. A line has one source and either a single destination or
/// a tree of branches fanning out to several destinations. `points` are
/// absolute canvas coordinates of the intermediate vertices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub name: Option<String>,
    pub src: Option<Endpoint>,
    pub points: Vec<Point>,
    pub dst: Option<Endpoint>,
    pub branches: Vec<Branch>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub points: Vec<Point>,
    pub dst: Option<Endpoint>,
    pub branches: Vec<Branch>,
}

/// A resolved source → destination pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Connection {
    pub src: Endpoint,
    pub dst: Endpoint,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    pub mask_type: Option<String>,
    pub description: Option<String>,
    /// Mask icon drawing commands (`disp('...')`, `plot(...)`, ...).
    pub display: Option<String>,
    pub parameters: Vec<MaskParameter>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaskParameter {
    pub name: String,
    pub prompt: Option<String>,
    /// Raw value (a MATLAB expression, or literal text for popups/edit-as-text).
    pub value: String,
    /// `edit`, `popup`, `checkbox`, ...
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamePlacement {
    #[default]
    Normal,
    Alternate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockStyle {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub show_name: bool,
    pub name_placement: NamePlacement,
    pub drop_shadow: bool,
    pub font_size: Option<f64>,
}

impl Default for BlockStyle {
    fn default() -> Self {
        BlockStyle {
            foreground: None,
            background: None,
            show_name: true,
            name_placement: NamePlacement::Normal,
            drop_shadow: false,
            font_size: None,
        }
    }
}

/// Free text (or rich text) placed on the canvas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub text: String,
    /// Either a point (older files) or a full box; stored as a rect whose
    /// right/bottom equal left/top for point-anchored annotations.
    pub position: Rect,
    pub rich_text: bool,
    pub properties: BTreeMap<String, String>,
}

impl System {
    pub fn block(&self, id: &BlockId) -> Option<&Block> {
        self.blocks.iter().find(|b| &b.id == id)
    }

    pub fn block_by_name(&self, name: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.name == name)
    }

    /// Every source → destination pair in this system, with branch trees
    /// flattened. Lines missing a source or destination are skipped.
    pub fn connections(&self) -> Vec<Connection> {
        fn collect(
            src: &Endpoint,
            dst: &Option<Endpoint>,
            branches: &[Branch],
            out: &mut Vec<Connection>,
        ) {
            if let Some(d) = dst {
                out.push(Connection {
                    src: src.clone(),
                    dst: d.clone(),
                });
            }
            for b in branches {
                collect(src, &b.dst, &b.branches, out);
            }
        }
        let mut out = Vec::new();
        for line in &self.lines {
            if let Some(src) = &line.src {
                collect(src, &line.dst, &line.branches, &mut out);
            }
        }
        out
    }

    /// Bounding box of all blocks, line vertices and annotations.
    pub fn bounds(&self) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        let mut add = |r: Rect| {
            acc = Some(match acc {
                Some(a) => a.union(&r),
                None => r,
            })
        };
        for b in &self.blocks {
            add(b.position);
        }
        fn branch_points(bs: &[Branch], out: &mut Vec<Point>) {
            for b in bs {
                out.extend(&b.points);
                branch_points(&b.branches, out);
            }
        }
        for l in &self.lines {
            let mut pts = l.points.clone();
            branch_points(&l.branches, &mut pts);
            for p in pts {
                add(Rect::new(p.x, p.y, p.x, p.y));
            }
        }
        for a in &self.annotations {
            add(a.position);
        }
        acc
    }
}

impl Model {
    /// Visit every system depth-first. The path is `/`-joined block names
    /// starting with the model name, as in Simulink (`model/Sub/Inner`).
    pub fn walk(&self) -> Vec<(String, &System)> {
        fn go<'a>(path: String, sys: &'a System, out: &mut Vec<(String, &'a System)>) {
            out.push((path.clone(), sys));
            for b in &sys.blocks {
                if let Some(sub) = &b.subsystem {
                    go(format!("{path}/{}", escape_name(&b.name)), sub, out);
                }
            }
        }
        let mut out = Vec::new();
        go(self.name.clone(), &self.root, &mut out);
        out
    }

    /// Resolve a system by the path of block names below the root.
    pub fn system_at(&self, path: &[&str]) -> Option<&System> {
        let mut sys = &self.root;
        for name in path {
            sys = sys.block_by_name(name)?.subsystem.as_deref()?;
        }
        Some(sys)
    }

    /// The chart implementing the block at `path` (block names below the
    /// root).
    pub fn chart_at(&self, path: &[&str]) -> Option<&Chart> {
        self.charts.iter().find(|c| {
            let names = stateflow::split_path(&c.name);
            names.len() == path.len() && names.iter().zip(path).all(|(a, b)| a == b)
        })
    }

    /// Total number of blocks across all levels.
    pub fn block_count(&self) -> usize {
        self.walk().iter().map(|(_, s)| s.blocks.len()).sum()
    }

    /// Histogram of `block_type` across all levels.
    pub fn block_type_counts(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for (_, sys) in self.walk() {
            for b in &sys.blocks {
                *out.entry(b.block_type.clone()).or_insert(0) += 1;
            }
        }
        out
    }
}

/// Simulink escapes `/` in block names as `//` inside paths.
pub fn escape_name(name: &str) -> String {
    name.replace('/', "//")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(block: &str, kind: PortKind, index: u32) -> Endpoint {
        Endpoint {
            block: block.into(),
            port: PortRef { kind, index },
        }
    }

    fn block(id: &str, name: &str, ty: &str) -> Block {
        Block {
            id: id.into(),
            block_type: ty.into(),
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

    #[test]
    fn connections_flatten_branches() {
        let sys = System {
            lines: vec![Line {
                src: Some(ep("1", PortKind::Out, 1)),
                branches: vec![
                    Branch {
                        dst: Some(ep("2", PortKind::In, 1)),
                        ..Default::default()
                    },
                    Branch {
                        dst: None,
                        branches: vec![Branch {
                            dst: Some(ep("3", PortKind::Enable, 1)),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let c = sys.connections();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].dst, ep("2", PortKind::In, 1));
        assert_eq!(c[1].dst, ep("3", PortKind::Enable, 1));
        assert!(c.iter().all(|c| c.src == ep("1", PortKind::Out, 1)));
    }

    #[test]
    fn walk_and_system_at() {
        let mut inner = block("2", "A/B", "SubSystem");
        inner.subsystem = Some(Box::new(System {
            blocks: vec![block("3", "G", "Gain")],
            ..Default::default()
        }));
        let model = Model {
            name: "m".into(),
            source: SourceFormat::Slx,
            simulink_version: None,
            config: SimConfig::default(),
            root: System {
                blocks: vec![block("1", "C", "Constant"), inner],
                ..Default::default()
            },
            workspace: BTreeMap::new(),
            type_defaults: Default::default(),
            charts: vec![],
        };
        let paths: Vec<_> = model.walk().into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec!["m", "m/A//B"]);
        assert_eq!(model.system_at(&["A/B"]).unwrap().blocks[0].name, "G");
        assert_eq!(model.block_count(), 3);
        assert_eq!(model.block_type_counts()["Gain"], 1);
    }

    #[test]
    fn chart_at_matches_escaped_paths() {
        let mut b = block("1", "x", "SubSystem");
        assert_eq!(b.stateflow_type(), None);
        b.parameters.insert("SFBlockType".into(), "NONE".into());
        assert_eq!(b.stateflow_type(), None);
        b.parameters
            .insert("SFBlockType".into(), "MATLAB Function".into());
        assert_eq!(b.stateflow_type(), Some("MATLAB Function"));

        let model = Model {
            name: "m".into(),
            source: SourceFormat::Slx,
            simulink_version: None,
            config: SimConfig::default(),
            root: System::default(),
            workspace: BTreeMap::new(),
            type_defaults: Default::default(),
            charts: vec![Chart {
                id: "5".into(),
                name: "Sub/a//b".into(),
                kind: ChartKind::MatlabFunction,
                states: vec![],
                transitions: vec![],
                junctions: vec![],
                data: vec![],
                script: Some("function y = f(u)".into()),
                update_method: None,
                sample_time: None,
            }],
        };
        assert_eq!(model.chart_at(&["Sub", "a/b"]).unwrap().id, "5");
        assert!(model.chart_at(&["Sub"]).is_none());
        assert!(model.chart_at(&["Sub", "a", "b"]).is_none());
    }

    #[test]
    fn port_counts_from_short_vector() {
        let p = PortCounts::from_slice(&[2, 1, 1]);
        assert_eq!(p.count(PortKind::In), 2);
        assert_eq!(p.count(PortKind::Enable), 1);
        assert_eq!(p.count(PortKind::Trigger), 0);
    }

    #[test]
    fn display_type_prefers_mask_then_library() {
        let mut b = block("1", "x", "Reference");
        b.library_source = Some("simulink/Discontinuities/Saturation".into());
        assert_eq!(b.display_type(), "Saturation");
        b.mask = Some(Mask {
            mask_type: Some("PID Controller".into()),
            ..Default::default()
        });
        assert_eq!(b.display_type(), "PID Controller");
    }

    #[test]
    fn model_json_roundtrip() {
        let model = Model {
            name: "m".into(),
            source: SourceFormat::Mdl,
            simulink_version: Some("7.4".into()),
            config: SimConfig {
                solver: Some("ode45".into()),
                ..Default::default()
            },
            root: System {
                blocks: vec![block("1", "G", "Gain")],
                lines: vec![Line {
                    src: Some(ep("1", PortKind::Out, 1)),
                    points: vec![Point::new(1.0, 2.0)],
                    dst: Some(ep("1", PortKind::In, 1)),
                    ..Default::default()
                }],
                ..Default::default()
            },
            workspace: BTreeMap::new(),
            type_defaults: Default::default(),
            charts: vec![],
        };
        let json = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(model, back);
    }
}
