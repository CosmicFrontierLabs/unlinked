//! Stateflow content: state charts, MATLAB Function blocks and truth tables.
//!
//! Each Stateflow-backed Simulink block has one [`Chart`] whose `name` is the
//! block's path below the model root. Graphical objects keep Stateflow's own
//! ids (`SSID`, or `id` in older files) and positions in the coordinate frame
//! of the view they are drawn in: the chart itself, or a subcharted state
//! (see [`State::subviewer`]).

use crate::{Point, Rect};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chart {
    pub id: String,
    /// Block path relative to the model, `/`-joined with `/` inside names
    /// escaped as `//`, e.g. `"LOS guidance/alpha,e"`.
    pub name: String,
    pub kind: ChartKind,
    pub states: Vec<State>,
    pub transitions: Vec<Transition>,
    pub junctions: Vec<Junction>,
    /// Chart-level data declarations (block inputs, outputs, parameters,
    /// locals) in file order.
    #[serde(default)]
    pub data: Vec<ChartData>,
    /// MATLAB code of a MATLAB Function block.
    pub script: Option<String>,
    /// Raw `updateMethod` (`chartUpdate` in MDL), e.g. `INHERITED`,
    /// `DISCRETE` or `CONTINUOUS`; `None` when the file does not say.
    #[serde(default)]
    pub update_method: Option<String>,
    /// Raw `sampleTime` expression, `None` when absent.
    #[serde(default)]
    pub sample_time: Option<String>,
}

/// A Stateflow data declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartData {
    pub id: String,
    pub name: String,
    pub scope: DataScope,
    /// 1-based block port for inputs and outputs, in declaration order.
    pub port: Option<u32>,
    /// Raw `props.array.size`, e.g. `"-1"` (inherited) or `"[3 1]"`.
    pub size: Option<String>,
    /// Raw `props.array.isDynamic` (`"1"` for variable-size data).
    pub variable_size: Option<String>,
    /// Raw `props.complexity`, e.g. `"SF_COMPLEX_INHERITED"`.
    pub complexity: Option<String>,
    /// Raw `dataType`, e.g. `"double"` or `"Inherit: Same as Simulink"`.
    pub data_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataScope {
    Input,
    Output,
    Parameter,
    Local,
    Constant,
    /// Any other scope, e.g. `DATA_STORE_MEMORY_DATA`.
    Other(String),
}

impl DataScope {
    /// Map Stateflow's data `scope` property.
    pub fn from_scope(s: Option<&str>) -> Self {
        match s.map(str::trim) {
            Some("INPUT_DATA") => DataScope::Input,
            Some("OUTPUT_DATA") => DataScope::Output,
            Some("PARAMETER_DATA") => DataScope::Parameter,
            None | Some("") | Some("LOCAL_DATA") => DataScope::Local,
            Some("CONSTANT_DATA") => DataScope::Constant,
            Some(other) => DataScope::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChartKind {
    StateChart,
    MatlabFunction,
    TruthTable,
    /// Any other Stateflow chart type, e.g. `STATE_EVENT_TABLE_CHART`.
    Other(String),
}

impl ChartKind {
    /// Map Stateflow's chart `type` property.
    pub fn from_type(t: Option<&str>) -> Self {
        match t.map(str::trim) {
            None | Some("") | Some("CHART") => ChartKind::StateChart,
            Some("EML_CHART") => ChartKind::MatlabFunction,
            Some("TRUTH_TABLE_CHART") => ChartKind::TruthTable,
            Some(other) => ChartKind::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    /// `labelString`: the state name on the first line, then actions.
    pub label: String,
    pub position: Rect,
    /// Enclosing state, `None` for top-level states.
    pub parent: Option<String>,
    /// View the state is drawn in: the chart id or a subcharted state id.
    pub subviewer: Option<String>,
    pub kind: StateKind,
    /// Embedded MATLAB code (MATLAB functions inside charts).
    pub script: Option<String>,
}

impl State {
    /// State name: the label's first line, up to any `/` action separator.
    pub fn name(&self) -> &str {
        let first = self.label.lines().next().unwrap_or("");
        first.split('/').next().unwrap_or(first).trim()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateKind {
    /// Exclusive (`OR_STATE`).
    Or,
    /// Parallel (`AND_STATE`).
    And,
    /// Graphical or MATLAB function (`FUNC_STATE`).
    Function,
    /// Box (`GROUP_STATE`).
    Group,
    /// Free-text note (`isNoteBox`); the label may be rich text (HTML).
    Note,
    Other(String),
}

impl StateKind {
    /// Map Stateflow's state `type` property.
    pub fn from_type(t: Option<&str>) -> Self {
        match t.map(str::trim) {
            None | Some("") | Some("OR_STATE") => StateKind::Or,
            Some("AND_STATE") => StateKind::And,
            Some("FUNC_STATE") => StateKind::Function,
            Some("GROUP_STATE") => StateKind::Group,
            Some(other) => StateKind::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    pub label: String,
    /// Source state or junction; `None` for a default transition.
    pub src: Option<String>,
    pub dst: Option<String>,
    /// Source intersection, midpoint and destination intersection, as
    /// recorded in the file.
    pub points: Vec<Point>,
    /// Top-left corner of the label.
    pub label_position: Option<Point>,
    pub subviewer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Junction {
    pub id: String,
    /// Bounding box of the junction circle.
    pub position: Rect,
    pub kind: JunctionKind,
    pub subviewer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JunctionKind {
    Connective,
    History,
    Other(String),
}

impl JunctionKind {
    pub fn from_type(t: Option<&str>) -> Self {
        match t.map(str::trim) {
            None | Some("") | Some("CONNECTIVE_JUNCTION") => JunctionKind::Connective,
            Some("HISTORY_JUNCTION") => JunctionKind::History,
            Some(other) => JunctionKind::Other(other.to_string()),
        }
    }
}

impl Chart {
    pub fn state(&self, id: &str) -> Option<&State> {
        self.states.iter().find(|s| s.id == id)
    }

    /// Whether `obj_view` (an object's `subviewer`) places it in `view`,
    /// where `None` is the chart's own top-level view.
    pub fn in_view(&self, obj_view: Option<&str>, view: Option<&str>) -> bool {
        let top = |v: Option<&str>| v.is_none_or(|v| v == self.id);
        match view {
            None => top(obj_view),
            Some(v) => obj_view == Some(v),
        }
    }

    /// Ids of the states that have their own view (subcharts with
    /// contents), collected in one pass over the chart.
    pub fn subchart_ids(&self) -> HashSet<&str> {
        self.states
            .iter()
            .map(|s| s.subviewer.as_deref())
            .chain(self.junctions.iter().map(|j| j.subviewer.as_deref()))
            .chain(self.transitions.iter().map(|t| t.subviewer.as_deref()))
            .flatten()
            .filter(|v| *v != self.id)
            .collect()
    }

    /// Follow subcharted state names from the top-level view, returning the
    /// id of the innermost view (`None` for the chart itself).
    pub fn view_at(&self, names: &[&str]) -> Option<Option<&str>> {
        let subcharts = self.subchart_ids();
        let mut view: Option<&str> = None;
        for name in names {
            let s = self.states.iter().find(|s| {
                self.in_view(s.subviewer.as_deref(), view)
                    && s.name() == *name
                    && subcharts.contains(s.id.as_str())
            })?;
            view = Some(&s.id);
        }
        Some(view)
    }
}

/// Split a Simulink block path into names, undoing [`crate::escape_name`].
pub fn split_path(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            if chars.peek() == Some(&'/') {
                chars.next();
                cur.push('/');
            } else {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: &str, label: &str, subviewer: &str) -> State {
        State {
            id: id.into(),
            label: label.into(),
            position: Rect::default(),
            parent: None,
            subviewer: Some(subviewer.into()),
            kind: StateKind::Or,
            script: None,
        }
    }

    #[test]
    fn data_scopes_map() {
        assert_eq!(DataScope::from_scope(Some("INPUT_DATA")), DataScope::Input);
        assert_eq!(DataScope::from_scope(None), DataScope::Local);
        assert_eq!(
            DataScope::from_scope(Some("FUNCTION_INPUT_DATA")),
            DataScope::Other("FUNCTION_INPUT_DATA".into())
        );
    }

    #[test]
    fn split_path_undoes_escaping() {
        assert_eq!(split_path("a/b//c/d"), vec!["a", "b/c", "d"]);
        assert_eq!(split_path("x"), vec!["x"]);
    }

    #[test]
    fn state_names_and_views() {
        let chart = Chart {
            id: "9".into(),
            name: "c".into(),
            kind: ChartKind::StateChart,
            states: vec![
                state("1", "Outer\nentry: x = 1;", "9"),
                state("2", "Inner/ during: y++;", "1"),
            ],
            transitions: vec![],
            junctions: vec![],
            data: vec![],
            script: None,
            update_method: None,
            sample_time: None,
        };
        assert_eq!(chart.states[0].name(), "Outer");
        assert_eq!(chart.states[1].name(), "Inner");
        assert_eq!(chart.subchart_ids(), HashSet::from(["1"]));
        assert_eq!(chart.view_at(&[]), Some(None));
        assert_eq!(chart.view_at(&["Outer"]), Some(Some("1")));
        assert_eq!(chart.view_at(&["Inner"]), None);
        assert!(chart.in_view(Some("9"), None));
        assert!(chart.in_view(None, None));
        assert!(chart.in_view(Some("1"), Some("1")));
        assert!(!chart.in_view(Some("1"), None));
    }
}
