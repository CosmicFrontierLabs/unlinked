//! Local Goto/From tags are signal aliases within a single subsystem.
//! Semantics: https://www.mathworks.com/help/simulink/slref/goto.html
//! Scoped/global tags remain errors until hierarchy-aware visibility is supported.
use crate::{block_error, Error};
use std::collections::BTreeMap;
use unlinked_model::*;

pub(super) fn lower(model: &Model) -> Result<Model, Error> {
    let mut model = model.clone();
    let mut remaining = 100_000usize;
    system(&mut model.root, 0, &mut remaining)?;
    Ok(model)
}
fn system(system: &mut System, depth: usize, remaining: &mut usize) -> Result<(), Error> {
    if depth > 64 {
        return Err(Error::Options("subsystem depth exceeds 64".into()));
    }
    *remaining = remaining
        .checked_sub(system.blocks.len())
        .ok_or_else(|| Error::Options("routing block budget exceeded".into()))?;
    let mut tags = BTreeMap::new();
    for block in &system.blocks {
        if block.block_type == "Goto" {
            if block.param("TagVisibility").unwrap_or("local") != "local" {
                return Err(block_error(
                    &block.id.0,
                    "only local Goto tag visibility is supported",
                ));
            }
            let tag = block
                .param("GotoTag")
                .filter(|t| !t.is_empty())
                .ok_or_else(|| block_error(&block.id.0, "missing Goto tag"))?;
            if tags.insert(tag.to_string(), block.id.clone()).is_some() {
                return Err(block_error(&block.id.0, "ambiguous local Goto tag"));
            }
        }
    }
    for block in &mut system.blocks {
        if block.block_type == "From" {
            let tag = block
                .param("GotoTag")
                .ok_or_else(|| block_error(&block.id.0, "missing From tag"))?;
            let source = tags
                .get(tag)
                .ok_or_else(|| block_error(&block.id.0, "no matching Goto in this subsystem"))?;
            system.lines.push(Line {
                src: Some(Endpoint {
                    block: source.clone(),
                    port: PortRef {
                        kind: PortKind::Out,
                        index: 1,
                    },
                }),
                dst: Some(Endpoint {
                    block: block.id.clone(),
                    port: PortRef {
                        kind: PortKind::In,
                        index: 1,
                    },
                }),
                ..Line::default()
            });
        }
        if matches!(block.block_type.as_str(), "Goto" | "From") {
            block.block_type = "Gain".into();
            block.parameters.insert("Gain".into(), "1".into());
            block.ports = PortCounts::from_slice(&[1, 1]);
        }
        if let Some(child) = block.subsystem.as_mut() {
            self::system(child, depth + 1, remaining)?;
        }
    }
    Ok(())
}
