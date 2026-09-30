//! Stateflow charts: `simulink/stateflow.xml` (or the split
//! `simulink/stateflow/machine.xml` + `chart_<id>.xml` parts of newer
//! releases) in SLX packages, and the top-level `Stateflow { ... }` section
//! of MDL files.
//!
//! SLX nests states inside their parent's `<Children>`; MDL lists every
//! object flat and links it to its chart (`chart`) and parent (`treeNode`).
//! Object properties are otherwise named the same, so one set of builders
//! reads both. Unknown elements are ignored.

use crate::convert::parse_numbers;
use crate::slx::SlxPackage;
use crate::tree::Node;
use crate::ImportError;
use std::collections::HashMap;
use unlinked_model::stateflow::split_path;
use unlinked_model::{
    escape_name, Block, Chart, ChartData, ChartKind, DataScope, Junction, JunctionKind, Point,
    Rect, State, StateKind, System, Transition,
};

const SINGLE_PART: &str = "simulink/stateflow.xml";
const MACHINE_PART: &str = "simulink/stateflow/machine.xml";

/// Read every chart in an SLX package; empty when it has no Stateflow.
pub fn read_slx(pkg: &mut SlxPackage) -> Result<Vec<Chart>, ImportError> {
    let doc = if pkg.has(SINGLE_PART) {
        pkg.read_xml(SINGLE_PART)?
    } else if pkg.has(MACHINE_PART) {
        let mut doc = pkg.read_xml(MACHINE_PART)?;
        inline_chart_parts(pkg, &mut doc)?;
        doc
    } else {
        return Ok(Vec::new());
    };
    Ok(xml_charts(&doc))
}

/// Replace `<chart Ref="chart_7"/>` placeholders with their part's content.
fn inline_chart_parts(pkg: &mut SlxPackage, doc: &mut Node) -> Result<(), ImportError> {
    for machine in doc.children.iter_mut().filter(|c| c.tag == "machine") {
        for list in machine.children.iter_mut().filter(|c| c.tag == "Children") {
            for chart in list.children.iter_mut().filter(|c| c.tag == "chart") {
                let Some(r) = chart.attr("Ref") else { continue };
                let part = format!("simulink/stateflow/{r}.xml");
                let node = pkg.read_xml(&part)?;
                if node.tag != "chart" {
                    return Err(ImportError::Xml(format!("{part}: expected <chart>")));
                }
                *chart = node;
            }
        }
    }
    Ok(())
}

/// Charts of a parsed `<Stateflow>` document.
pub fn xml_charts(doc: &Node) -> Vec<Chart> {
    let names = instance_names(doc);
    let mut out = Vec::new();
    for machine in doc.children_named("machine") {
        for list in machine.children_named("Children") {
            for node in list.children_named("chart") {
                let mut chart = new_chart(node, &names);
                if let Some(children) = node.child("Children") {
                    xml_objects(children, None, &mut chart);
                }
                finish(&mut chart, node);
                out.push(chart);
            }
        }
    }
    out
}

fn xml_objects(list: &Node, parent: Option<&str>, chart: &mut Chart) {
    for n in &list.children {
        match n.tag.as_str() {
            "state" => {
                let s = state(n, parent.map(str::to_string));
                let id = s.id.clone();
                chart.states.push(s);
                if let Some(children) = n.child("Children") {
                    xml_objects(children, Some(&id), chart);
                }
            }
            "transition" => chart.transitions.push(transition(n)),
            "junction" => chart.junctions.push(junction(n)),
            "data" if parent.is_none() => chart.data.push(data(n)),
            _ => {}
        }
    }
}

/// Charts of an MDL `Stateflow` section.
pub fn mdl_charts(section: &Node) -> Vec<Chart> {
    let names = instance_names(section);
    let mut charts: Vec<Chart> = section
        .children_named("chart")
        .map(|n| new_chart(n, &names))
        .collect();
    let index: HashMap<String, usize> = charts
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), i))
        .collect();
    for n in &section.children {
        // Data records name their owner (chart, state or machine) only
        // through `linkNode`; chart-level data is linked to the chart.
        let owner = if n.tag == "data" {
            first_token(n.get("linkNode"))
        } else {
            n.get("chart").map(|c| c.trim().to_string())
        };
        let Some(&i) = owner.and_then(|c| index.get(&c)) else {
            continue;
        };
        let chart = &mut charts[i];
        match n.tag.as_str() {
            "state" => {
                let parent = first_token(n.get("treeNode")).filter(|p| p != "0" && *p != chart.id);
                chart.states.push(state(n, parent));
            }
            "transition" => chart.transitions.push(transition(n)),
            "junction" => chart.junctions.push(junction(n)),
            "data" => chart.data.push(data(n)),
            _ => {}
        }
    }
    for (chart, node) in charts.iter_mut().zip(section.children_named("chart")) {
        finish(chart, node);
    }
    charts
}

/// Make chart names relative to the model root. Charts are named by block
/// path, which some files prefix with the model name.
pub fn relativize(charts: &mut [Chart], model_name: &str, root: &System) {
    let mut index = BlockIndex::default();
    for chart in charts {
        let names = split_path(&chart.name);
        if index.exists(root, &names) {
            continue;
        }
        if names.len() > 1 && names[0] == model_name && index.exists(root, &names[1..]) {
            chart.name = names[1..]
                .iter()
                .map(|n| escape_name(n))
                .collect::<Vec<_>>()
                .join("/");
        }
    }
}

/// Block-name lookup per system, built on first use so resolving many
/// chart paths stays linear in the model size.
#[derive(Default)]
struct BlockIndex<'a> {
    systems: HashMap<*const System, HashMap<&'a str, &'a Block>>,
}

impl<'a> BlockIndex<'a> {
    fn block(&mut self, sys: &'a System, name: &str) -> Option<&'a Block> {
        self.systems
            .entry(sys as *const System)
            .or_insert_with(|| {
                let mut m = HashMap::new();
                for b in &sys.blocks {
                    m.entry(b.name.as_str()).or_insert(b);
                }
                m
            })
            .get(name)
            .copied()
    }

    fn exists(&mut self, root: &'a System, names: &[String]) -> bool {
        let Some((last, parents)) = names.split_last() else {
            return false;
        };
        let mut sys = root;
        for name in parents {
            match self.block(sys, name).and_then(|b| b.subsystem.as_deref()) {
                Some(s) => sys = s,
                None => return false,
            }
        }
        self.block(sys, last).is_some()
    }
}

/// `chart id -> block path` from `instance` records.
fn instance_names(node: &Node) -> HashMap<String, String> {
    node.children_named("instance")
        .filter_map(|i| {
            Some((
                i.get("chart")?.trim().to_string(),
                i.get("name")?.to_string(),
            ))
        })
        .collect()
}

fn new_chart(node: &Node, names: &HashMap<String, String>) -> Chart {
    let id = object_id(node);
    let name = names
        .get(&id)
        .map(String::as_str)
        .or_else(|| node.get("name"))
        .unwrap_or_default()
        .to_string();
    Chart {
        id,
        name,
        kind: ChartKind::from_type(node.get("type")),
        states: Vec::new(),
        transitions: Vec::new(),
        junctions: Vec::new(),
        data: Vec::new(),
        script: None,
        update_method: opt_string(node.get("updateMethod").or_else(|| node.get("chartUpdate"))),
        sample_time: opt_string(node.get("sampleTime")),
    }
}

/// A MATLAB Function block's code lives on the chart's `eml` record in
/// some releases and on its single function state in others. Inputs and
/// outputs take block ports in declaration order.
fn finish(chart: &mut Chart, node: &Node) {
    chart.script = script(node);
    if chart.script.is_none() && chart.kind == ChartKind::MatlabFunction {
        chart.script = chart.states.iter().find_map(|s| s.script.clone());
    }
    let (mut inputs, mut outputs) = (0, 0);
    for d in &mut chart.data {
        let counter = match d.scope {
            DataScope::Input => &mut inputs,
            DataScope::Output => &mut outputs,
            _ => continue,
        };
        *counter += 1;
        d.port = Some(*counter);
    }
}

fn data(n: &Node) -> ChartData {
    let props = n.child("props");
    let array = props.and_then(|p| p.child("array"));
    ChartData {
        id: object_id(n),
        name: n.get("name").unwrap_or_default().trim().to_string(),
        scope: DataScope::from_scope(n.get("scope")),
        port: None,
        size: array.and_then(|a| opt_string(a.get("size"))),
        variable_size: array.and_then(|a| opt_string(a.get("isDynamic"))),
        complexity: props.and_then(|p| opt_string(p.get("complexity"))),
        data_type: opt_string(n.get("dataType")),
    }
}

fn object_id(n: &Node) -> String {
    n.get("SSID")
        .or_else(|| n.get("id"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn first_token(s: Option<&str>) -> Option<String> {
    s?.trim()
        .trim_start_matches('[')
        .split(|c: char| c == ',' || c.is_whitespace())
        .find(|t| !t.is_empty())
        .map(|t| t.trim_end_matches(']').to_string())
}

fn numbers(s: Option<&str>) -> Vec<f64> {
    s.map(parse_numbers)
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v.is_finite())
        .collect()
}

fn script(n: &Node) -> Option<String> {
    n.child("eml")
        .and_then(|e| e.get("script"))
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn opt_string(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn state(n: &Node, parent: Option<String>) -> State {
    let p = numbers(n.get("position"));
    let position = if p.len() >= 4 {
        Rect::new(p[0], p[1], p[0] + p[2].max(0.0), p[1] + p[3].max(0.0))
    } else {
        Rect::default()
    };
    State {
        id: object_id(n),
        label: n.get("labelString").unwrap_or_default().to_string(),
        position,
        parent,
        subviewer: opt_string(n.get("subviewer")),
        kind: if n.get("isNoteBox").map(str::trim) == Some("1") {
            StateKind::Note
        } else {
            StateKind::from_type(n.get("type"))
        },
        script: script(n),
    }
}

/// End point of a transition from its `intersection` vector, whose 5th and
/// 6th entries are the canvas coordinates.
fn intersection(end: Option<&Node>) -> Option<Point> {
    let v = numbers(end?.get("intersection"));
    (v.len() >= 6).then(|| Point::new(v[4], v[5]))
}

fn transition(n: &Node) -> Transition {
    let src = n.child("src");
    let dst = n.child("dst");
    let end_id = |e: Option<&Node>| e.map(object_id).filter(|id| !id.is_empty() && id != "0");
    let mid = numbers(n.get("midPoint"));
    let label = numbers(n.get("labelPosition"));
    let points = [
        intersection(src),
        (mid.len() >= 2).then(|| Point::new(mid[0], mid[1])),
        intersection(dst),
    ]
    .into_iter()
    .flatten()
    .collect();
    Transition {
        id: object_id(n),
        label: n.get("labelString").unwrap_or_default().to_string(),
        src: end_id(src),
        dst: end_id(dst),
        points,
        label_position: (label.len() >= 2).then(|| Point::new(label[0], label[1])),
        subviewer: opt_string(n.get("subviewer")),
    }
}

fn junction(n: &Node) -> Junction {
    let p = numbers(n.get("position"));
    let position = match p.as_slice() {
        [x, y, r, ..] => Rect::new(x - r.abs(), y - r.abs(), x + r.abs(), y + r.abs()),
        [x, y] => Rect::new(x - 5.0, y - 5.0, x + 5.0, y + 5.0),
        _ => Rect::default(),
    };
    Junction {
        id: object_id(n),
        position,
        kind: JunctionKind::from_type(n.get("type")),
        subviewer: opt_string(n.get("subviewer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slx::parse_xml;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Stateflow>
  <machine id="1">
    <Children>
      <target id="2" name="sfun"/>
      <chart id="3">
        <P Name="name">Controller/Mode logic</P>
        <P Name="updateMethod">DISCRETE</P>
        <P Name="sampleTime">Ts</P>
        <Children>
          <state SSID="1">
            <P Name="labelString">Off
entry: y = 0;</P>
            <P Name="position">[10 20 100 50]</P>
            <P Name="subviewer">3</P>
            <P Name="type">OR_STATE</P>
            <Children>
              <state SSID="5">
                <P Name="labelString">Idle</P>
                <P Name="position">[20 40 30 20]</P>
                <P Name="type">AND_STATE</P>
              </state>
              <data SSID="12" name="arg"><P Name="scope">FUNCTION_INPUT_DATA</P></data>
            </Children>
          </state>
          <state SSID="6">
            <P Name="labelString">&lt;html&gt;&lt;body&gt;&lt;p&gt;Note&lt;/p&gt;&lt;/body&gt;&lt;/html&gt;</P>
            <P Name="position">[0 100 50 20]</P>
            <P Name="isNoteBox">1</P>
            <P Name="type">GROUP_STATE</P>
          </state>
          <data SSID="9" name="y"><P Name="scope">OUTPUT_DATA</P></data>
          <data SSID="10" name="u">
            <P Name="scope">INPUT_DATA</P>
            <props>
              <array><P Name="size">[3 1]</P><P Name="isDynamic">1</P></array>
              <P Name="complexity">SF_COMPLEX_NO</P>
            </props>
            <P Name="dataType">double</P>
          </data>
          <data SSID="11" name="v"><P Name="scope">INPUT_DATA</P></data>
          <mystery><P Name="x">1</P></mystery>
          <transition SSID="2">
            <P Name="labelString">[u &gt; 1]</P>
            <P Name="labelPosition">[50 5 40 12]</P>
            <src><P Name="intersection">[0 0 1 0 60 5 0 0]</P></src>
            <dst>
              <P Name="SSID">1</P>
              <P Name="intersection">[1 0 -1 0 60 20 0 0]</P>
            </dst>
            <P Name="midPoint">[60 12.5]</P>
          </transition>
          <junction SSID="4">
            <P Name="position">[200 30 7]</P>
            <P Name="type">HISTORY_JUNCTION</P>
          </junction>
        </Children>
      </chart>
      <chart id="7">
        <P Name="type">EML_CHART</P>
        <Children>
          <state SSID="1">
            <P Name="labelString">eML_blk_kernel()</P>
            <P Name="type">FUNC_STATE</P>
            <eml><P Name="script">function y = f(u)
y = 2*u;</P></eml>
          </state>
        </Children>
      </chart>
    </Children>
  </machine>
  <instance id="8">
    <P Name="name">Controller/f</P>
    <P Name="chart">7</P>
  </instance>
</Stateflow>"#;

    #[test]
    fn slx_charts_parse() {
        let doc = parse_xml(XML, &mut 10_000).unwrap();
        let charts = xml_charts(&doc);
        assert_eq!(charts.len(), 2);

        let c = &charts[0];
        assert_eq!(c.name, "Controller/Mode logic");
        assert_eq!(c.kind, ChartKind::StateChart);
        assert_eq!(c.states.len(), 3);
        assert_eq!(c.states[2].kind, StateKind::Note);
        let data: Vec<_> = c
            .data
            .iter()
            .map(|d| (d.name.as_str(), d.scope.clone(), d.port))
            .collect();
        assert_eq!(
            data,
            vec![
                ("y", DataScope::Output, Some(1)),
                ("u", DataScope::Input, Some(1)),
                ("v", DataScope::Input, Some(2)),
            ]
        );
        assert_eq!(c.data[1].size.as_deref(), Some("[3 1]"));
        assert_eq!(c.data[1].data_type.as_deref(), Some("double"));
        assert_eq!(c.data[2].size, None);
        assert_eq!(c.data[1].variable_size.as_deref(), Some("1"));
        assert_eq!(c.data[1].complexity.as_deref(), Some("SF_COMPLEX_NO"));
        assert_eq!(c.data[2].variable_size, None);
        assert_eq!(c.data[2].complexity, None);
        assert_eq!(c.update_method.as_deref(), Some("DISCRETE"));
        assert_eq!(c.sample_time.as_deref(), Some("Ts"));
        assert_eq!(charts[1].update_method, None);
        assert_eq!(charts[1].sample_time, None);
        assert_eq!(c.states[0].name(), "Off");
        assert_eq!(c.states[0].position, Rect::new(10.0, 20.0, 110.0, 70.0));
        assert_eq!(c.states[0].subviewer.as_deref(), Some("3"));
        assert_eq!(c.states[1].parent.as_deref(), Some("1"));
        assert_eq!(c.states[1].kind, StateKind::And);
        let t = &c.transitions[0];
        assert_eq!(t.label, "[u > 1]");
        assert_eq!(t.src, None);
        assert_eq!(t.dst.as_deref(), Some("1"));
        assert_eq!(
            t.points,
            vec![
                Point::new(60.0, 5.0),
                Point::new(60.0, 12.5),
                Point::new(60.0, 20.0)
            ]
        );
        assert_eq!(t.label_position, Some(Point::new(50.0, 5.0)));
        let j = &c.junctions[0];
        assert_eq!(j.kind, JunctionKind::History);
        assert_eq!(j.position, Rect::new(193.0, 23.0, 207.0, 37.0));

        let f = &charts[1];
        assert_eq!(f.name, "Controller/f");
        assert_eq!(f.kind, ChartKind::MatlabFunction);
        assert_eq!(f.script.as_deref(), Some("function y = f(u)\ny = 2*u;"));
    }

    #[test]
    fn mdl_charts_parse() {
        let text = r#"
Stateflow {
  machine {
    id 1
    name "m"
  }
  chart {
    id 2
    name "m/Chart"
    chartUpdate INHERITED
    treeNode [0 3 0 0]
  }
  state {
    id 3
    labelString "A\nentry: x = 1;"
    position [10 10 100 60]
    chart 2
    treeNode [2 4 0 0]
    subviewer 2
    type OR_STATE
  }
  state {
    id 4
    labelString "B"
    position [20 30 40 20]
    chart 2
    treeNode [3 0 0 0]
    subviewer 2
  }
  transition {
    id 5
    labelString "e[x>0]"
    src {
      id 3
      intersection [1 0 -1 0 110 30 0 0]
    }
    dst {
      id 6
      intersection [1 0 -1 0 150 30 0 0]
    }
    midPoint [130 30]
    chart 2
    linkNode [2 0 0]
  }
  junction {
    id 6
    position [157 30 7]
    chart 2
    type CONNECTIVE_JUNCTION
  }
  unknown {
    chart 2
  }
  data {
    id 8
    name "u"
    linkNode [2 9 0]
    scope INPUT_DATA
    props {
      array {
        size "2"
      }
    }
    dataType "single"
  }
  data {
    id 9
    name "local"
    linkNode [3 0 8]
    scope LOCAL_DATA
  }
  instance {
    id 7
    name "Chart"
    machine 1
    chart 2
  }
}
"#;
        let sections = crate::mdl::parse(text).unwrap();
        let charts = mdl_charts(&sections[0]);
        assert_eq!(charts.len(), 1);
        let c = &charts[0];
        assert_eq!(c.name, "Chart");
        assert_eq!(c.states.len(), 2);
        assert_eq!(c.states[0].parent, None);
        assert_eq!(c.states[0].label, "A\nentry: x = 1;");
        assert_eq!(c.states[1].parent.as_deref(), Some("3"));
        assert_eq!(c.transitions[0].src.as_deref(), Some("3"));
        assert_eq!(c.transitions[0].dst.as_deref(), Some("6"));
        assert_eq!(c.transitions[0].points.len(), 3);
        assert_eq!(c.junctions[0].kind, JunctionKind::Connective);
        assert_eq!(c.update_method.as_deref(), Some("INHERITED"));
        assert_eq!(c.data.len(), 1, "state-level data is not chart data");
        assert_eq!(c.data[0].name, "u");
        assert_eq!(c.data[0].port, Some(1));
        assert_eq!(c.data[0].size.as_deref(), Some("2"));
        assert_eq!(c.data[0].data_type.as_deref(), Some("single"));
    }

    #[test]
    fn model_prefix_is_stripped() {
        let mut sub = unlinked_model::System::default();
        sub.blocks.push(test_block("a/b"));
        let mut outer = test_block("Sub");
        outer.subsystem = Some(Box::new(sub));
        let root = System {
            blocks: vec![outer],
            ..Default::default()
        };
        let mut charts = vec![
            Chart {
                name: "m/Sub/a//b".into(),
                ..empty()
            },
            Chart {
                name: "Sub".into(),
                ..empty()
            },
        ];
        relativize(&mut charts, "m", &root);
        assert_eq!(charts[0].name, "Sub/a//b");
        assert_eq!(charts[1].name, "Sub");
    }

    fn slx(parts: &[(&str, &str)]) -> SlxPackage {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            w.start_file(*name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        SlxPackage::open(&w.finish().unwrap().into_inner()).unwrap()
    }

    #[test]
    fn split_chart_parts_resolve_or_error() {
        let machine = r#"<Stateflow><machine id="1"><Children>
            <chart Ref="chart_7"/>
          </Children></machine></Stateflow>"#;
        let chart = r#"<chart id="7"><P Name="name">Sub</P><P Name="type">EML_CHART</P></chart>"#;

        let mut ok = slx(&[
            (MACHINE_PART, machine),
            ("simulink/stateflow/chart_7.xml", chart),
        ]);
        let charts = read_slx(&mut ok).unwrap();
        assert_eq!(charts.len(), 1);
        assert_eq!(charts[0].name, "Sub");
        assert_eq!(charts[0].kind, ChartKind::MatlabFunction);

        let mut missing = slx(&[(MACHINE_PART, machine)]);
        assert!(matches!(
            read_slx(&mut missing),
            Err(ImportError::MissingPart(_))
        ));

        let mut wrong = slx(&[
            (MACHINE_PART, machine),
            ("simulink/stateflow/chart_7.xml", "<System/>"),
        ]);
        assert!(matches!(read_slx(&mut wrong), Err(ImportError::Xml(_))));
    }

    #[test]
    fn relativize_scales_linearly() {
        let n = 20_000;
        let root = System {
            blocks: (0..n).map(|i| test_block(&format!("b{i}"))).collect(),
            ..Default::default()
        };
        let mut charts: Vec<Chart> = (0..n)
            .map(|i| Chart {
                name: format!("m/b{i}"),
                ..empty()
            })
            .collect();
        let start = std::time::Instant::now();
        relativize(&mut charts, "m", &root);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(charts[n - 1].name, format!("b{}", n - 1));
    }

    fn empty() -> Chart {
        Chart {
            id: "1".into(),
            name: String::new(),
            kind: ChartKind::StateChart,
            states: vec![],
            transitions: vec![],
            junctions: vec![],
            data: vec![],
            script: None,
            update_method: None,
            sample_time: None,
        }
    }

    fn test_block(name: &str) -> unlinked_model::Block {
        unlinked_model::Block {
            id: name.into(),
            block_type: "SubSystem".into(),
            name: name.into(),
            position: Rect::default(),
            orientation: Default::default(),
            mirrored: false,
            ports: Default::default(),
            parameters: Default::default(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: Default::default(),
        }
    }
}
