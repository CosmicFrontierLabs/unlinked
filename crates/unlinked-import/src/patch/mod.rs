//! Write edits back into the original model file.
//!
//! Edits are applied to a faithful representation of the file rather than
//! regenerated from the IR, so everything the IR does not model (masks,
//! configuration sets, Stateflow, styling, unknown elements) is preserved.

mod dom;
mod expansion;
mod mdl;
mod slx;

use crate::{decode_text, import, ImportError};
use std::collections::HashMap;
use unlinked_model::boundary::{boundary_remap, BoundaryRemap};
use unlinked_model::catalog::interface_port_number;
use unlinked_model::edit::{structural_regression, system_names, Edit};
use unlinked_model::{Block, BlockId, PortCounts, PortKind};

/// An edit with the names its targets have in the file at that point of
/// the batch, and what it produced in the IR.
struct Resolved {
    edit: Edit,
    route: Option<RouteUpdate>,
    hierarchy: Option<unlinked_model::hierarchy::CreatePlan>,
    expansion: Option<unlinked_model::expand::ExpandPlan>,
    source_line_count: usize,
    /// Subsystem block names from the root.
    system: Vec<String>,
    /// Names of the system's blocks before the edit.
    names: HashMap<BlockId, String>,
    /// The block an `AddBlock` created.
    added: Option<Block>,
    /// New port counts of the edited block, when the edit changed them.
    ports: Option<PortCounts>,
    /// New port numbers of other port blocks in the system: their `Port`
    /// and, for bus elements, the interface `PortNumber`.
    renumbered: Vec<(BlockId, String)>,
    /// How the edit changes the ports of the subsystem it is inside.
    boundary: Option<Boundary>,
}

/// A subsystem block whose ports an edit inside it changes.
struct Boundary {
    /// Subsystem block names from the root to the system holding the block.
    system: Vec<String>,
    /// The subsystem block's name there.
    name: String,
    remap: BoundaryRemap,
    /// Its port counts afterwards.
    ports: PortCounts,
}

impl Resolved {
    /// Current name of block `id` in the edited system.
    fn name(&self, id: &BlockId) -> Result<&str, ImportError> {
        self.names
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| ImportError::Edit(format!("block {id} not found")))
    }
}

/// Apply `edits` in order to the model in `bytes`, returning the new file.
///
/// Every edit is first checked against the imported IR, so an edit that
/// refers to a missing block, carries an invalid value or adds a structural
/// error is rejected before anything is written.
pub fn apply_edits(filename: &str, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ImportError> {
    let original = import(filename, bytes)?;
    let mut model = original.clone();
    let mut resolved = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        // Files locate systems and blocks by name, so resolve the edit's
        // IDs against the model as earlier edits in the batch left it.
        let failed = |message: String| ImportError::Edit(format!("edit {index}: {message}"));
        let system = system_names(&model, edit.system())
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let path: Vec<&str> = system.iter().map(String::as_str).collect();
        let sys = model
            .system_at(&path)
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let names = sys
            .blocks
            .iter()
            .map(|b| (b.id.clone(), b.name.clone()))
            .collect();
        let ports_before = edit.block().and_then(|id| sys.block(id)).map(|b| b.ports);
        // Deleting or renumbering a port block renumbers its siblings.
        let ports_numbered: Vec<(BlockId, Option<u32>)> = match edit {
            Edit::DeleteBlock { .. } => sys
                .blocks
                .iter()
                .map(|b| (b.id.clone(), interface_port_number(b).ok()))
                .collect(),
            Edit::SetParameter { name, .. } if name == "Port" => sys
                .blocks
                .iter()
                .map(|b| (b.id.clone(), interface_port_number(b).ok()))
                .collect(),
            _ => Vec::new(),
        };
        // The subsystem block's identity before the edit, if its ports change.
        let remap = boundary_remap(&model, edit).map_err(|e| failed(e.to_string()))?;
        let boundary_at = remap
            .as_ref()
            .map(|r| {
                let outer = system_names(&model, &r.parent_system)
                    .ok_or_else(|| failed("no system around the subsystem".into()))?;
                let name = system.last().cloned().unwrap_or_default();
                Ok::<_, ImportError>((outer, name))
            })
            .transpose()?;
        let source_line_count = sys.lines.len();
        let hierarchy = match edit {
            Edit::CreateSubsystem {
                system,
                ids,
                id,
                name,
            } => Some(
                unlinked_model::hierarchy::plan_create(&model, system, ids, id, name)
                    .map_err(|e| failed(e.to_string()))?,
            ),
            _ => None,
        };
        let expansion = match edit {
            Edit::ExpandSubsystem { system, id } => Some(
                unlinked_model::expand::plan_expand(&model, system, id)
                    .map_err(|e| failed(e.to_string()))?,
            ),
            _ => None,
        };
        edit.apply(&mut model).map_err(|e| failed(e.to_string()))?;
        let sys = system_names(&model, edit.system())
            .and_then(|names| {
                let path: Vec<&str> = names.iter().map(String::as_str).collect();
                model.system_at(&path)
            })
            .ok_or_else(|| failed(format!("no subsystem at {:?}", edit.system())))?;
        let added = match edit {
            Edit::AddBlock { id, .. } => sys.block(id).cloned(),
            _ => None,
        };
        let ports = edit
            .block()
            .and_then(|id| sys.block(id))
            .map(|b| b.ports)
            .filter(|p| ports_before.is_some_and(|before| before != *p));
        let route = match edit {
            Edit::SetRoute { dst, .. } => {
                Some(route_update(sys, dst, false).map_err(|e| failed(e.to_string()))?)
            }
            Edit::SetTrunkRoute { src, .. } => {
                Some(route_update(sys, src, true).map_err(|e| failed(e.to_string()))?)
            }
            _ => None,
        };
        let renumbered = ports_numbered
            .into_iter()
            .filter_map(|(id, before)| {
                let now = interface_port_number(sys.block(&id)?).ok()?;
                (before != Some(now)).then(|| (id, now.to_string()))
            })
            .collect();
        let boundary = match (remap, boundary_at) {
            (Some(remap), Some((outer, name))) => {
                let path: Vec<&str> = outer.iter().map(String::as_str).collect();
                let ports = model
                    .system_at(&path)
                    .and_then(|s| s.block(&remap.parent))
                    .map(|b| b.ports)
                    .ok_or_else(|| failed("the subsystem block is missing".into()))?;
                Some(Boundary {
                    system: outer,
                    name,
                    remap,
                    ports,
                })
            }
            _ => None,
        };
        resolved.push(Resolved {
            edit: edit.clone(),
            route,
            hierarchy,
            expansion,
            source_line_count,
            system,
            names,
            added,
            ports,
            renumbered,
            boundary,
        });
    }
    structural_regression(&original, &model).map_err(|e| ImportError::Edit(e.to_string()))?;

    if bytes.starts_with(b"PK\x03\x04") {
        return slx::apply(bytes, &resolved);
    }
    let utf8 = std::str::from_utf8(bytes).is_ok();
    let text = mdl::apply(&decode_text(bytes), &resolved)?;
    if utf8 {
        Ok(text.into_bytes())
    } else {
        encode_cp1252(&text)
    }
}

/// Where a solver setting is written.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConfigPlace {
    /// The configuration set's solver component.
    Component,
    /// The model's own properties.
    Model,
}

/// Where to write each property that setting `key` changes, so it lands
/// where the importer reads it: the solver component wins, and model-level
/// properties fill in what it lacks. `component` and `model` count each
/// property's occurrences there; `component` is `None` without a solver
/// component. Duplicated properties are refused, as their effective value
/// is ambiguous.
fn config_places(
    key: &str,
    component: Option<&dyn Fn(&str) -> usize>,
    model: &dyn Fn(&str) -> usize,
) -> Result<Vec<(&'static str, ConfigPlace)>, ImportError> {
    let in_component = |k: &str| component.map_or(0, |c| c(k));
    unlinked_model::edit::config_writes(key, |k| in_component(k) + model(k) > 0)
        .into_iter()
        .map(|k| {
            let (c, m) = (in_component(k), model(k));
            if c > 1 || m > 1 {
                return Err(ImportError::Edit(format!(
                    "the solver setting {k} is stored more than once"
                )));
            }
            let place = if c == 1 || (m == 0 && component.is_some()) {
                ConfigPlace::Component
            } else {
                ConfigPlace::Model
            };
            Ok((k, place))
        })
        .collect()
}

/// The `Ports` value for `p`: counts in Simulink's order, trailing zeros
/// dropped.
fn format_ports(p: &PortCounts) -> String {
    let mut v = [
        p.inputs, p.outputs, p.enable, p.trigger, p.state, p.lconn, p.rconn, p.ifaction, p.reset,
    ]
    .to_vec();
    while v.last() == Some(&0) {
        v.pop();
    }
    let parts: Vec<String> = v.iter().map(u32::to_string).collect();
    format!("[{}]", parts.join(", "))
}

/// Parse an SLX-style endpoint `12#out:1` into its SID and port.
fn parse_endpoint(v: &str, default_kind: PortKind) -> Option<(&str, unlinked_model::PortRef)> {
    let (sid, port) = v.split_once('#')?;
    Some((sid, crate::convert::parse_port(port, default_kind)?))
}

/// Inverse of the windows-1252 decoding used on import, for files that were
/// not UTF-8. Characters windows-1252 cannot represent are rejected.
fn encode_cp1252(text: &str) -> Result<Vec<u8>, ImportError> {
    text.chars()
        .map(|c| {
            (0u8..=255).find(|&b| crate::cp1252(b) == c).ok_or_else(|| {
                ImportError::Edit(format!(
                    "{c:?} cannot be stored in this windows-1252 encoded file"
                ))
            })
        })
        .collect()
}

struct RouteUpdate {
    src: unlinked_model::Endpoint,
    points: RoutePoints,
}
struct RoutePoints {
    value: String,
    branches: Vec<RoutePoints>,
}
fn route_update(
    sys: &unlinked_model::System,
    endpoint: &unlinked_model::Endpoint,
    trunk: bool,
) -> Result<RouteUpdate, unlinked_model::edit::EditError> {
    use unlinked_model::{Branch, Point};
    fn points(
        start: Point,
        vertices: &[Point],
        branches: &[Branch],
    ) -> Result<RoutePoints, unlinked_model::edit::EditError> {
        let mut prev = start;
        let mut parts = Vec::with_capacity(vertices.len());
        for p in vertices {
            let (dx, dy) = (p.x - prev.x, p.y - prev.y);
            if !dx.is_finite() || !dy.is_finite() {
                return Err(unlinked_model::edit::EditError::Invalid(
                    "route relative coordinates overflow".into(),
                ));
            }
            parts.push(format!("{dx}, {dy}"));
            prev = *p;
        }
        Ok(RoutePoints {
            value: format!("[{}]", parts.join("; ")),
            branches: branches
                .iter()
                .map(|b| points(prev, &b.points, &b.branches))
                .collect::<Result<_, _>>()?,
        })
    }
    let (root, _) = unlinked_model::route_edit::route_location(sys, endpoint, trunk)?;
    let line = &sys.lines[root];
    let src = line.src.as_ref().expect("route checked source");
    let block = sys.block(&src.block).expect("route checked block");
    Ok(RouteUpdate {
        src: src.clone(),
        points: points(
            unlinked_model::geometry::port_anchor(block, src.port),
            &line.points,
            &line.branches,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlinked_model::{BlockId, Rect};

    const MDL: &str = "Model {\n  Name\t\"m\"\n  System {\n    Name\t\"m\"\n    Block {\n      BlockType\tGain\n      Name\t\"g\"\n      Position\t[10, 10, 40, 40]\n    }\n  }\n}\n";

    fn gain_id() -> BlockId {
        import("m.mdl", MDL.as_bytes()).unwrap().root.blocks[0]
            .id
            .clone()
    }

    #[test]
    fn edits_are_validated_before_writing() {
        let bad = Edit::MoveBlock {
            system: vec![],
            id: "nope".into(),
            position: Rect::new(0.0, 0.0, 10.0, 10.0),
        };
        assert!(matches!(
            apply_edits("m.mdl", MDL.as_bytes(), &[bad]),
            Err(ImportError::Edit(_))
        ));
        let inverted = Edit::MoveBlock {
            system: vec![],
            id: gain_id(),
            position: Rect::new(10.0, 10.0, 0.0, 0.0),
        };
        assert!(apply_edits("m.mdl", MDL.as_bytes(), &[inverted]).is_err());
    }

    #[test]
    fn mdl_edit_reimports() {
        let out = apply_edits(
            "m.mdl",
            MDL.as_bytes(),
            &[
                Edit::SetParameter {
                    system: vec![],
                    id: gain_id(),
                    name: "Gain".into(),
                    value: "7".into(),
                },
                Edit::RenameBlock {
                    system: vec![],
                    id: gain_id(),
                    name: "k".into(),
                },
            ],
        )
        .unwrap();
        let model = import("m.mdl", &out).unwrap();
        assert_eq!(model.root.blocks[0].name, "k");
        assert_eq!(model.root.blocks[0].param("Gain"), Some("7"));
    }

    #[test]
    fn structural_edits_write_simulink_sections() {
        use unlinked_model::edit::{apply_batch, next_sid};
        use unlinked_model::{Endpoint, PortRef};
        let src = MDL.replace(
            "Name\t\"m\"\n    Block",
            "Name\t\"m\"\n    SIDHighWatermark\t\"7\"\n    Block",
        );
        let model = import("m.mdl", src.as_bytes()).unwrap();
        let sid = next_sid(&model).unwrap().to_string();
        assert_eq!(sid, "8");
        let port = |block: &BlockId, kind, index| Endpoint {
            block: block.clone(),
            port: PortRef { kind, index },
        };
        let edits = [
            Edit::AddBlock {
                system: vec![],
                id: sid.as_str().into(),
                block_type: "Sum".into(),
                name: "add".into(),
                position: Rect::new(100.0, 10.0, 130.0, 40.0),
            },
            Edit::Connect {
                system: vec![],
                src: port(&gain_id(), PortKind::Out, 1),
                dst: port(&sid.as_str().into(), PortKind::In, 2),
            },
        ];
        let out = apply_edits("m.mdl", src.as_bytes(), &edits).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.contains(
            "    Block {\n      BlockType\tSum\n      Name\t\"add\"\n      SID\t\"8\"\n      Ports\t[2, 1]\n"
        ), "{text}");
        assert!(text.contains(
            "    Line {\n      SrcBlock\t\"g\"\n      SrcPort\t1\n      DstBlock\t\"add\"\n      DstPort\t2\n    }\n"
        ), "{text}");
        assert!(text.contains("SIDHighWatermark\t\"8\""), "{text}");
        let mut expected = model;
        apply_batch(&mut expected, &edits).unwrap();
        assert_eq!(import("m.mdl", &out).unwrap().root, expected.root);

        // Disconnecting a line's only destination removes the line.
        let more = [Edit::Disconnect {
            system: vec![],
            dst: port(&sid.as_str().into(), PortKind::In, 2),
        }];
        let out = apply_edits("m.mdl", &out, &more).unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("Line {"));
    }

    #[test]
    fn orientation_keeps_the_legacy_form_until_a_mirror_needs_rotation() {
        use unlinked_model::Orientation;
        let orient = |orientation, mirrored| {
            [Edit::SetOrientation {
                system: vec![],
                id: gain_id(),
                orientation,
                mirrored,
            }]
        };
        let left = apply_edits("m.mdl", MDL.as_bytes(), &orient(Orientation::Left, false)).unwrap();
        let text = String::from_utf8(left.clone()).unwrap();
        assert!(text.contains("Orientation\t\"left\"") && !text.contains("BlockRotation"));
        let block = &import("m.mdl", &left).unwrap().root.blocks[0];
        assert_eq!(
            (block.orientation, block.mirrored),
            (Orientation::Left, false)
        );

        let up = apply_edits("m.mdl", &left, &orient(Orientation::Up, true)).unwrap();
        let text = String::from_utf8(up.clone()).unwrap();
        assert!(
            text.contains("BlockRotation\t90") && text.contains("BlockMirror\ton"),
            "{text}"
        );
        assert!(!text.contains("Orientation\t"), "{text}");
        let block = &import("m.mdl", &up).unwrap().root.blocks[0];
        assert_eq!((block.orientation, block.mirrored), (Orientation::Up, true));
    }

    #[test]
    fn mdl_routes_are_written_as_bare_matrices() {
        let out_id = import("m.mdl", MDL_LINE.as_bytes()).unwrap().root.blocks[1]
            .id
            .clone();
        let route = Edit::SetRoute {
            system: vec![],
            dst: unlinked_model::Endpoint {
                block: out_id,
                port: unlinked_model::PortRef {
                    kind: PortKind::In,
                    index: 1,
                },
            },
            points: vec![unlinked_model::Point::new(70.0, 25.0)],
        };
        let out = apply_edits("m.mdl", MDL_LINE.as_bytes(), &[route]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Points\t[25, 0]\n"), "{text}");
    }

    const MDL_LINE: &str = "Model {\n  Name\t\"m\"\n  System {\n    Name\t\"m\"\n    Block {\n      BlockType\tGain\n      Name\t\"g\"\n      SID\t\"1\"\n      Position\t[10, 10, 40, 40]\n    }\n    Block {\n      BlockType\tOutport\n      Name\t\"out\"\n      SID\t\"2\"\n      Position\t[100, 10, 130, 40]\n    }\n    Line {\n      SrcBlock\t\"g\"\n      SrcPort\t1\n      Points\t[20, 0]\n      DstBlock\t\"out\"\n      DstPort\t1\n    }\n  }\n}\n";

    #[test]
    fn cp1252_roundtrips() {
        let bytes = b"\x80 caf\xe9 \x93q\x94";
        assert_eq!(encode_cp1252(&decode_text(bytes)).unwrap(), bytes);
        assert!(matches!(encode_cp1252("漢字"), Err(ImportError::Edit(_))));
    }
}
