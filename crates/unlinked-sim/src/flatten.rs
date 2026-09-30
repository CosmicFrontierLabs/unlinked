//! Lower ordinary virtual subsystem boundaries to scalar identity blocks.
use super::{block_error, Error};
use std::collections::BTreeMap;
use unlinked_model::*;

pub fn flatten(model: &Model) -> Result<Model, Error> {
    fn id(prefix: &str, id: &BlockId) -> BlockId {
        BlockId(format!("{prefix}{}", id.0.replace('/', "//")))
    }
    fn system(
        sys: &System,
        prefix: &str,
        display_prefix: &str,
        depth: usize,
        out: &mut System,
    ) -> Result<BTreeMap<(String, PortKind, u32), Endpoint>, Error> {
        if depth > 64 {
            return Err(Error::Options("subsystem depth exceeds 64".into()));
        }
        let mut aliases = BTreeMap::new();
        for block in &sys.blocks {
            if out.blocks.len() >= 100_000 {
                return Err(Error::Options("flattened graph budget exceeded".into()));
            }
            if let Some(sub) = &block.subsystem {
                if block.block_type != "SubSystem"
                    || block.mask.is_some()
                    || block.library_source.is_some()
                    || block.param("TreatAsAtomicUnit").is_some_and(|v| v != "off")
                {
                    return Err(block_error(
                        &block.id.0,
                        "only unmasked virtual subsystems can be flattened",
                    ));
                }
                if block.ports.enable
                    + block.ports.trigger
                    + block.ports.reset
                    + block.ports.ifaction
                    + block.ports.state
                    + block.ports.lconn
                    + block.ports.rconn
                    > 0
                {
                    return Err(block_error(
                        &block.id.0,
                        "conditional or physical subsystems are unsupported",
                    ));
                }
                let child_prefix = format!("{}/", id(prefix, &block.id).0);
                let child_display = if display_prefix.is_empty() {
                    block.name.clone()
                } else {
                    format!("{display_prefix}/{}", block.name)
                };
                system(sub, &child_prefix, &child_display, depth + 1, out)?;
                for port in &sub.blocks {
                    let kind = match port.block_type.as_str() {
                        "Inport" => PortKind::In,
                        "Outport" => PortKind::Out,
                        _ => continue,
                    };
                    let index = port
                        .param("Port")
                        .unwrap_or("1")
                        .parse::<u32>()
                        .map_err(|_| block_error(&port.id.0, "invalid subsystem port number"))?;
                    if index == 0 {
                        return Err(block_error(
                            &port.id.0,
                            "subsystem port numbers are 1-based",
                        ));
                    }
                    let endpoint = Endpoint {
                        block: id(&child_prefix, &port.id),
                        port: PortRef { kind, index: 1 },
                    };
                    if aliases
                        .insert((block.id.0.clone(), kind, index), endpoint)
                        .is_some()
                    {
                        return Err(block_error(
                            &block.id.0,
                            "duplicate subsystem boundary port",
                        ));
                    }
                }
            } else {
                let mut node = block.clone();
                node.id = id(prefix, &block.id);
                if !display_prefix.is_empty() {
                    node.name = format!("{display_prefix}/{}", block.name);
                }
                if depth > 0 && ["Inport", "Outport"].contains(&node.block_type.as_str()) {
                    // Boundary interfaces are identity nodes. Keep type/sample parameters
                    // so the regular compiler can reject unsupported signal semantics.
                    node.block_type = "Gain".into();
                    node.parameters.insert("Gain".into(), "1".into());
                    node.ports = PortCounts::from_slice(&[1, 1]);
                }
                out.blocks.push(node);
            }
        }
        let resolve = |ep: &Endpoint| -> Result<Endpoint, Error> {
            if let Some(target) = aliases.get(&(ep.block.0.clone(), ep.port.kind, ep.port.index)) {
                return Ok(target.clone());
            }
            if sys.block(&ep.block).is_some_and(|b| b.subsystem.is_some()) {
                return Err(block_error(
                    &ep.block.0,
                    "connection references missing subsystem interface port",
                ));
            }
            let mut ep = ep.clone();
            ep.block = id(prefix, &ep.block);
            Ok(ep)
        };
        for connection in sys.connections() {
            out.lines.push(Line {
                src: Some(resolve(&connection.src)?),
                dst: Some(resolve(&connection.dst)?),
                ..Line::default()
            });
        }
        Ok(aliases)
    }
    if !model.root.blocks.iter().any(|b| b.subsystem.is_some()) {
        return Ok(model.clone());
    }
    let mut result = model.clone();
    result.root = System::default();
    system(&model.root, "", "", 0, &mut result.root)?;
    Ok(result)
}
