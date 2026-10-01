//! Conservative copy/paste of native palette blocks.
//!
//! External connections are deliberately omitted. Internal connections are
//! rerouted, not translated; names of internal signal trees are preserved.
use crate::catalog::{self, PortResolution};
use crate::edit::{apply_batch, next_sid, system_names, Edit, EditError, SystemRef};
use crate::{BlockId, BlockStyle, Model, Orientation, Point, PortKind};
use std::collections::{BTreeMap, BTreeSet};

const MAX_EDITS: usize = 2000;
const MAX_SELECTION: usize = 256;

/// Build an atomic duplicate action without modifying `model`.
///
/// Until the edit vocabulary can preserve their metadata, styled,
/// masked, linked, hierarchical, interface and chart-owning blocks are refused.
/// Implicit catalog defaults are materialized to avoid substituting the palette's
/// creation profile (notably Switch and ZeroOrderHold) for imported behavior.
pub fn duplicate(
    model: &Model,
    system: &SystemRef,
    ids: &[BlockId],
    offset: Point,
    first_sid: u64,
) -> Result<Vec<Edit>, EditError> {
    let invalid = |message: &str| EditError::Invalid(message.into());
    if !offset.x.is_finite() || !offset.y.is_finite() {
        return Err(invalid("copy offset must be finite"));
    }
    if ids.len() > MAX_SELECTION {
        return Err(invalid("copy selection exceeds 256 blocks"));
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Bound recursive helpers and the clone/apply validation below first.
    if crate::validation::validate_structure(model).truncated {
        return Err(invalid("model exceeds the structural validation budget"));
    }
    if first_sid < next_sid(model).ok_or_else(|| invalid("no SIDs left"))? {
        return Err(invalid("copy first SID is already reserved"));
    }
    first_sid
        .checked_add(ids.len() as u64 - 1)
        .ok_or_else(|| invalid("copy SID allocation overflows"))?;
    let mut sys = &model.root;
    for id in system {
        if sys.blocks.iter().filter(|block| block.id == *id).count() != 1 {
            return Err(invalid(
                "copy system path contains an ambiguous or missing ID",
            ));
        }
        sys = sys
            .block(id)
            .and_then(|b| b.subsystem.as_deref())
            .ok_or_else(|| EditError::NoSystem(system.clone()))?;
    }
    let selected: BTreeSet<_> = ids.iter().collect();
    if selected.len() != ids.len() {
        return Err(invalid("copy selection contains repeated block IDs"));
    }
    let mut names: BTreeSet<_> = sys.blocks.iter().map(|b| b.name.clone()).collect();
    let mut mapped = BTreeMap::new();
    let mut edits = Vec::new();
    let mut expected = Vec::new();
    let path = system_names(model, system).ok_or_else(|| EditError::NoSystem(system.clone()))?;
    for (index, id) in ids.iter().enumerate() {
        let block = sys
            .block(id)
            .ok_or_else(|| EditError::NoBlock(id.clone()))?;
        let reject =
            |reason: &str| EditError::Invalid(format!("cannot copy {:?}: {reason}", block.name));
        let descriptor = catalog::find(&block.block_type)
            .filter(|d| d.creatable)
            .ok_or_else(|| reject("not a native palette block"))?;
        if block.mask.is_some()
            || block.library_source.is_some()
            || block.subsystem.is_some()
            || block.interface.is_some()
            || descriptor.source_block.is_some()
            || matches!(
                block.block_type.as_str(),
                "Inport"
                    | "Outport"
                    | "EnablePort"
                    | "TriggerPort"
                    | "ActionPort"
                    | "ResetPort"
                    | "PMIOPort"
            )
        {
            return Err(reject(
                "masked, linked, subsystem or interface blocks are not supported",
            ));
        }
        let chart_path: Vec<_> = path
            .iter()
            .cloned()
            .chain(std::iter::once(block.name.clone()))
            .collect();
        if model
            .charts
            .iter()
            .any(|c| crate::stateflow::split_path(&c.name).starts_with(&chart_path))
        {
            return Err(reject("block owns a Stateflow chart"));
        }
        if block.style != BlockStyle::default() {
            return Err(reject("copying nondefault block styles is not supported"));
        }
        if block.parameters.len() + descriptor.parameters.len() + edits.len() + 1 > MAX_EDITS {
            return Err(invalid("copy exceeds the 2000 edit budget"));
        }
        if sys
            .blocks
            .iter()
            .filter(|candidate| candidate.id == *id)
            .count()
            != 1
        {
            return Err(reject("block ID is ambiguous in this system"));
        }
        let mut parameters = block.parameters.clone();
        for parameter in descriptor.parameters {
            if !parameters.contains_key(parameter.name) {
                let value = parameter.implicit_default.ok_or_else(|| {
                    reject(&format!("unknown implicit default for {}", parameter.name))
                })?;
                parameters.insert(parameter.name.into(), value.into());
            }
        }
        let PortResolution::Known(ports) = descriptor.resolve_ports(&parameters) else {
            return Err(reject("port counts must be resolved before copying"));
        };
        if ports != block.ports {
            return Err(reject("declared ports disagree with the block parameters"));
        }
        let new_id = BlockId((first_sid + index as u64).to_string());
        mapped.insert(id.clone(), new_id.clone());
        let mut suffix = 1usize;
        let name = loop {
            let name = format!("{} copy{suffix}", block.name);
            if names.insert(name.clone()) {
                break name;
            }
            suffix += 1;
        };
        let mut position = block.position;
        position.left += offset.x;
        position.right += offset.x;
        position.top += offset.y;
        position.bottom += offset.y;
        edits.push(Edit::AddBlock {
            system: system.clone(),
            id: new_id.clone(),
            block_type: block.block_type.clone(),
            name,
            position,
        });
        if block.orientation != Orientation::Right || block.mirrored {
            edits.push(Edit::SetOrientation {
                system: system.clone(),
                id: new_id.clone(),
                orientation: block.orientation,
                mirrored: block.mirrored,
            });
        }
        for (name, value) in &parameters {
            edits.push(Edit::SetParameter {
                system: system.clone(),
                id: new_id.clone(),
                name: name.clone(),
                value: value.clone(),
            });
        }
        expected.push((new_id, parameters, ports, block.orientation, block.mirrored));
        if edits.len() > MAX_EDITS {
            return Err(invalid("copy exceeds the 2000 edit budget"));
        }
    }
    let mut signal_names = Vec::new();
    // Preserve one shared name on each copied internal signal tree.
    for line in &sys.lines {
        if line.name.as_ref().is_some_and(|name| !name.is_empty())
            && line
                .src
                .as_ref()
                .is_some_and(|src| selected.contains(&src.block))
        {
            let mut destinations = vec![line.dst.as_ref()];
            let mut pending: Vec<_> = line.branches.iter().collect();
            while let Some(branch) = pending.pop() {
                destinations.push(branch.dst.as_ref());
                pending.extend(&branch.branches);
            }
            if destinations
                .into_iter()
                .flatten()
                .any(|dst| selected.contains(&dst.block))
            {
                let source = line.src.as_ref().expect("selected source above");
                crate::route_edit::route_location(sys, source, true)?;
                let mut src = source.clone();
                src.block = mapped[&source.block].clone();
                signal_names.push(Edit::SetSignalName {
                    system: system.clone(),
                    src,
                    name: line.name.clone().expect("named line above"),
                });
            }
        }
    }
    for connection in sys.connections() {
        let (Some(src), Some(dst)) = (
            mapped.get(&connection.src.block),
            mapped.get(&connection.dst.block),
        ) else {
            continue;
        };
        if connection.src.port.kind != PortKind::Out || connection.dst.port.kind != PortKind::In {
            return Err(invalid("copy supports only ordinary signal connections"));
        }
        let mut source = connection.src;
        let mut destination = connection.dst;
        source.block = src.clone();
        destination.block = dst.clone();
        edits.push(Edit::Connect {
            system: system.clone(),
            src: source,
            dst: destination,
        });
        if edits.len() > MAX_EDITS {
            return Err(invalid("copy exceeds the 2000 edit budget"));
        }
    }
    edits.extend(signal_names);
    if edits.len() > MAX_EDITS {
        return Err(invalid("copy exceeds the 2000 edit budget"));
    }
    let mut preview = model.clone();
    apply_batch(&mut preview, &edits).map_err(|error| error.error)?;
    let mut output = &preview.root;
    for id in system {
        output = output
            .block(id)
            .and_then(|b| b.subsystem.as_deref())
            .ok_or_else(|| EditError::NoSystem(system.clone()))?;
    }
    for (id, parameters, ports, orientation, mirrored) in expected {
        let block = output
            .block(&id)
            .ok_or_else(|| EditError::NoBlock(id.clone()))?;
        if block.parameters != parameters
            || block.ports != ports
            || block.orientation != orientation
            || block.mirrored != mirrored
        {
            return Err(invalid(
                "duplicate edit vocabulary cannot preserve these block parameters",
            ));
        }
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Endpoint, PortRef, Rect, SimConfig, SourceFormat, System};

    fn model() -> Model {
        let mut model = Model {
            name: "copy".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: SimConfig::default(),
            root: System::default(),
            workspace: BTreeMap::new(),
            charts: vec![],
        };
        for (id, kind, name) in [
            ("1", "Constant", "source"),
            ("2", "Gain", "gain"),
            ("3", "Gain", "outside"),
        ] {
            Edit::AddBlock {
                system: vec![],
                id: id.into(),
                block_type: kind.into(),
                name: name.into(),
                position: Rect {
                    left: 10.,
                    top: 20.,
                    right: 40.,
                    bottom: 50.,
                },
            }
            .apply(&mut model)
            .unwrap();
        }
        for dst in ["2", "3"] {
            Edit::Connect {
                system: vec![],
                src: endpoint("1", PortKind::Out),
                dst: endpoint(dst, PortKind::In),
            }
            .apply(&mut model)
            .unwrap();
        }
        model
    }
    fn endpoint(id: &str, kind: PortKind) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef { kind, index: 1 },
        }
    }
    fn copy(model: &Model, ids: &[&str]) -> Result<Vec<Edit>, EditError> {
        duplicate(
            model,
            &vec![],
            &ids.iter().map(|id| BlockId::from(*id)).collect::<Vec<_>>(),
            Point { x: 50., y: -10. },
            4,
        )
    }
    #[test]
    fn copies_parameters_and_internal_branch_only_atomically() {
        let mut source = model();
        source.root.blocks[1]
            .parameters
            .insert("Gain".into(), "K".into());
        source.root.blocks[1]
            .parameters
            .insert("UserData".into(), "preserve me".into());
        let original = source.clone();
        let edits = copy(&source, &["1", "2"]).unwrap();
        assert_eq!(source, original);
        apply_batch(&mut source, &edits).unwrap();
        let gain = source.root.block(&"5".into()).unwrap();
        assert_eq!(gain.parameters, original.root.blocks[1].parameters);
        assert_eq!(gain.position.left, 60.);
        assert_eq!(gain.position.top, 10.);
        assert_eq!(gain.name, "gain copy1");
        let connections = source.root.connections();
        assert_eq!(connections.len(), 3);
        assert!(connections
            .iter()
            .any(|c| c.src.block.0 == "4" && c.dst.block.0 == "5"));
    }
    #[test]
    fn absent_imported_switch_parameters_keep_implicit_behavior() {
        let mut source = model();
        source.root.blocks.truncate(1);
        source.root.lines.clear();
        let block = &mut source.root.blocks[0];
        block.block_type = "Switch".into();
        block.parameters.clear();
        block.ports = match catalog::find("Switch")
            .unwrap()
            .resolve_ports(&block.parameters)
        {
            PortResolution::Known(ports) => ports,
            _ => panic!("known"),
        };
        let edits = copy(&source, &["1"]).unwrap();
        apply_batch(&mut source, &edits).unwrap();
        let copied = source.root.block(&"4".into()).unwrap();
        assert_eq!(copied.parameters["Criteria"], "u2 >= Threshold");
        assert_eq!(copied.parameters["ZeroCross"], "on");
    }
    #[test]
    fn duplicate_preserves_all_orientation_and_mirror_states() {
        for orientation in [
            Orientation::Right,
            Orientation::Left,
            Orientation::Up,
            Orientation::Down,
        ] {
            for mirrored in [false, true] {
                let mut source = model();
                source.root.blocks[0].orientation = orientation;
                source.root.blocks[0].mirrored = mirrored;
                let edits = copy(&source, &["1"]).unwrap();
                apply_batch(&mut source, &edits).unwrap();
                let block = source.root.block(&"4".into()).unwrap();
                assert_eq!((block.orientation, block.mirrored), (orientation, mirrored));
            }
        }
    }
    #[test]
    fn rejects_metadata_that_cannot_be_preserved() {
        let mut source = model();
        source.root.blocks[0].style.background = Some("red".into());
        assert!(copy(&source, &["1"])
            .unwrap_err()
            .to_string()
            .contains("styles"));
        source.root.blocks[0].style = BlockStyle::default();
        source.root.blocks[0].orientation = Orientation::Left;
        assert!(copy(&source, &["1"]).is_ok());
        source.root.blocks[0].orientation = Orientation::Right;
        source.root.lines[0].name = Some("label".into());
        let edits = copy(&source, &["1", "2"]).unwrap();
        let mut named_copy = source.clone();
        apply_batch(&mut named_copy, &edits).unwrap();
        assert_eq!(
            named_copy.root.lines.last().unwrap().name.as_deref(),
            Some("label")
        );
        // External labels belong to the excluded external connection.
        assert!(copy(&source, &["1"]).is_ok());
        source.root.blocks[0].library_source = Some("library/source".into());
        assert!(copy(&source, &["1"]).is_err());
    }
    #[test]
    fn deterministic_names_and_bounds() {
        let mut source = model();
        source.root.blocks[2].name = "source copy1".into();
        let edits = copy(&source, &["1"]).unwrap();
        assert!(matches!(&edits[0], Edit::AddBlock{name,..} if name == "source copy2"));
        assert!(copy(&source, &["1", "1"]).is_err());
        assert!(duplicate(
            &source,
            &vec![],
            &["1".into(), "2".into()],
            Point { x: 0., y: 0. },
            u64::MAX
        )
        .is_err());
        assert!(duplicate(
            &source,
            &vec![],
            &["1".into()],
            Point { x: f64::NAN, y: 0. },
            4
        )
        .is_err());
        assert!(duplicate(&source, &vec![], &["1".into()], Point { x: 0., y: 0. }, 2).is_err());
    }
}
