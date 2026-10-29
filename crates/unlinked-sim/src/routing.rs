//! Local Goto/From tags are signal aliases within a single subsystem.
//! Semantics: https://www.mathworks.com/help/simulink/slref/goto.html
//! Scoped/global tags remain errors until hierarchy-aware visibility is supported.
//!
//! Commented blocks are removed by the later `commented` pass, so their
//! contents are never routed or validated here (Simulink excludes everything
//! inside a commented subsystem); a commented-out Goto still claims its tag so
//! that its From blocks end up grounded rather than unmatched.
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
    // Tags of commented-out Gotos: never validated, and claimed only when no
    // live Goto owns the tag, so the From blocks they fed read ground.
    let mut grounded_tags = BTreeMap::new();
    for block in &system.blocks {
        // Routing blocks become identity nodes below, which would let a
        // commented-through Goto or From pass its port-count check; Simulink
        // refuses it because the signal has no paired input and output port.
        if matches!(block.block_type.as_str(), "Goto" | "From")
            && block.commented() == Ok(Commented::Through)
        {
            return Err(block_error(
                &block.id.0,
                format!(
                    "Commented=through is refused for {} blocks: routing blocks have no paired input and output ports",
                    block.block_type
                ),
            ));
        }
        if block.block_type == "Goto" {
            if block.commented() == Ok(Commented::Out) {
                if let Some(tag) = block.param("GotoTag").filter(|t| !t.is_empty()) {
                    grounded_tags
                        .entry(tag.to_string())
                        .or_insert_with(|| block.id.clone());
                }
                continue;
            }
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
    for (tag, id) in grounded_tags {
        tags.entry(tag).or_insert(id);
    }
    for block in &mut system.blocks {
        let commented = matches!(block.commented(), Ok(Commented::Out | Commented::Through));
        if block.block_type == "From" {
            // A commented-out From is removed before it reads anything, so a
            // missing or unmatched tag is no error for it.
            let source = match block.param("GotoTag").map(|tag| tags.get(tag)) {
                Some(Some(source)) => Some(source.clone()),
                None | Some(None) if commented => None,
                None => return Err(block_error(&block.id.0, "missing From tag")),
                Some(None) => {
                    return Err(block_error(
                        &block.id.0,
                        "no matching Goto in this subsystem",
                    ))
                }
            };
            if let Some(source) = source {
                system.lines.push(Line {
                    src: Some(Endpoint {
                        block: source,
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
        }
        if matches!(block.block_type.as_str(), "Goto" | "From") {
            block.block_type = "Gain".into();
            block.parameters.insert("Gain".into(), "1".into());
            block.ports = PortCounts::from_slice(&[1, 1]);
        }
        // The contents of a commented subsystem are excluded from simulation
        // along with it, so they are neither routed nor validated.
        if commented {
            continue;
        }
        if let Some(child) = block.subsystem.as_mut() {
            self::system(child, depth + 1, remaining)?;
        }
    }
    Ok(())
}
