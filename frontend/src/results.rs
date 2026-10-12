//! Immutable recorded results shared between the simulation and diagram panes.
use std::rc::Rc;
use unlinked_model::{Endpoint, Model, PortKind, System};
use uuid::Uuid;
use yew::Callback;

#[derive(Clone, PartialEq)]
pub struct ShownSignal {
    pub id: String,
    pub name: String,
    pub values: Rc<Vec<f64>>,
}

#[derive(Clone, PartialEq)]
pub struct ShownRun {
    pub run_id: Uuid,
    pub version: i32,
    pub time: Rc<Vec<f64>>,
    pub signals: Rc<Vec<ShownSignal>>,
    /// The selected recorded sample (cursor B), initially the final sample.
    pub sample_index: usize,
}

#[derive(Clone, PartialEq)]
pub struct ResultsContext {
    pub run: Option<Rc<ShownRun>>,
    pub shown_version: i32,
    pub dirty: bool,
    /// Select this recorded signal ID and show the simulation pane.
    pub on_plot: Callback<String>,
}

impl ShownRun {
    /// Resolve a displayed output to recorded scalar components. Numeric
    /// Simulink SIDs have an unambiguous lowering vocabulary: virtual scopes
    /// join SIDs with '/', multiple outputs add ':out:N', and arrays add
    /// '[i]' or '[row,col]'. Subsystem outputs alias their internal Outport.
    ///
    /// Models with custom/non-numeric IDs are deliberately omitted: their
    /// IDs can collide with generated names, which the compiler renames.
    /// Without stored origin metadata, guessing those aliases is unsafe.
    pub fn signal_indices(&self, model: &Model, path: &[String], source: &Endpoint) -> Vec<usize> {
        if source.port.kind != PortKind::Out || source.port.index == 0 || !numeric_ids(&model.root)
        {
            return Vec::new();
        }
        let mut system = &model.root;
        let mut prefix = String::new();
        for name in path {
            let Some(block) = system.blocks.iter().find(|b| b.name == *name) else {
                return Vec::new();
            };
            let Some(child) = &block.subsystem else {
                return Vec::new();
            };
            prefix += &format!("{}/", block.id.0);
            system = child;
        }
        let Some(block) = system.block(&source.block) else {
            return Vec::new();
        };
        let base = if let Some(child) = &block.subsystem {
            let mut outports = child.blocks.iter().filter(|b| {
                b.block_type == "Outport"
                    && b.param("Port").unwrap_or("1").parse::<u32>().ok() == Some(source.port.index)
            });
            let Some(port) = outports.next() else {
                return Vec::new();
            };
            if outports.next().is_some() {
                return Vec::new();
            }
            format!("{prefix}{}/{}", block.id.0, port.id.0)
        } else if block.ports.outputs > 1 {
            if source.port.index > block.ports.outputs {
                return Vec::new();
            }
            format!("{prefix}{}:out:{}", block.id.0, source.port.index)
        } else if source.port.index == 1 {
            format!("{prefix}{}", block.id.0)
        } else {
            return Vec::new();
        };
        self.signals
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let suffix = s.id.strip_prefix(&base)?;
                (suffix.is_empty() || component_suffix(suffix)).then_some(i)
            })
            .collect()
    }
}

fn numeric_ids(system: &System) -> bool {
    system.blocks.iter().all(|b| {
        !b.id.0.is_empty()
            && b.id.0.bytes().all(|c| c.is_ascii_digit())
            && b.subsystem.as_ref().is_none_or(|s| numeric_ids(s))
    })
}
fn component_suffix(suffix: &str) -> bool {
    let Some(indices) = suffix.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let parts: Vec<_> = indices.split(',').collect();
    (1..=2).contains(&parts.len())
        && parts
            .iter()
            .all(|p| p.parse::<usize>().is_ok_and(|n| n > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlinked_model::PortRef;
    fn compiled_run(model: &Model) -> ShownRun {
        let graph = unlinked_sim::compile_with_inputs(
            model,
            &unlinked_sim::Options::default(),
            &Default::default(),
        )
        .unwrap();
        ShownRun {
            run_id: Uuid::nil(),
            version: 1,
            time: Rc::new(vec![0.0]),
            signals: Rc::new(
                graph
                    .nodes
                    .iter()
                    .map(|n| ShownSignal {
                        id: n.id.clone(),
                        name: n.name.clone(),
                        values: Rc::new(vec![0.0]),
                    })
                    .collect(),
            ),
            sample_index: 0,
        }
    }
    fn out(id: &str, index: u32) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef {
                kind: PortKind::Out,
                index,
            },
        }
    }
    #[test]
    fn matches_compiler_vector_components_and_multiple_outputs() {
        let mdl = "Model {\n Name m\n System {\n Name m\n Block {\n BlockType Constant\n Name c\n SID 1\n Ports [0,1]\n Value [1 2]\n }\n Block {\n BlockType Demux\n Name d\n SID 2\n Ports [1,2]\n Outputs 2\n }\n Line {\n SrcBlock c\n SrcPort 1\n DstBlock d\n DstPort 1\n }\n }\n}";
        let m = unlinked_import::import_mdl("m.mdl", mdl).unwrap();
        let run = compiled_run(&m);
        let ids = |ep| {
            run.signal_indices(&m, &[], &ep)
                .iter()
                .map(|&i| run.signals[i].id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(out("1", 1)), ["1[1]", "1[2]"]);
        assert_eq!(ids(out("2", 1)), ["2:out:1"]);
        assert_eq!(ids(out("2", 2)), ["2:out:2"]);
        assert!(ids(out("2", 3)).is_empty());
    }
    #[test]
    fn matches_virtual_subsystem_output_alias() {
        let mdl = "Model {\n Name m\n System {\n Name m\n Block {\n BlockType SubSystem\n Name sub\n SID 1\n Ports [0,1]\n System {\n Name sub\n Block {\n BlockType Constant\n Name c\n SID 2\n Ports [0,1]\n Value 3\n }\n Block {\n BlockType Outport\n Name y\n SID 3\n Ports [1,0]\n Port 1\n }\n Line {\n SrcBlock c\n SrcPort 1\n DstBlock y\n DstPort 1\n }\n }\n }\n }\n}";
        let mut m = unlinked_import::import_mdl("m.mdl", mdl).unwrap();
        let run = compiled_run(&m);
        let indices = run.signal_indices(&m, &[], &out("1", 1));
        assert_eq!(indices.len(), 1);
        assert_eq!(run.signals[indices[0]].id, "1/3");
        let indices = run.signal_indices(&m, &["sub".into()], &out("2", 1));
        assert_eq!(run.signals[indices[0]].id, "1/2");
        m.root.blocks[0].id.0 = "custom:out:1".into();
        assert!(run
            .signal_indices(&m, &[], &out("custom:out:1", 1))
            .is_empty());
    }
    #[test]
    fn array_suffixes_do_not_match_unrelated_generated_ids() {
        for valid in ["[1]", "[2,3]"] {
            assert!(component_suffix(valid));
        }
        for invalid in ["[0]", "[1]#1", "[1,2,3]", ":$state:1", "1", "[]"] {
            assert!(!component_suffix(invalid));
        }
    }
}
