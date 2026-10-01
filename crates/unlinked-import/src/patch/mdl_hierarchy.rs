//! Move raw MDL sections across a new virtual subsystem boundary.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::hierarchy::{CreatePlan, LineRecipe};
use unlinked_model::{BlockId, Endpoint};

fn recipe_index(recipe: &LineRecipe) -> usize {
    match recipe {
        LineRecipe::Keep { original_index } | LineRecipe::Rewrite { original_index, .. } => {
            *original_index
        }
    }
}
fn ordinary(s: &Section, root: bool) -> bool {
    (root || (s.prop("Src").is_none() && s.prop("SrcBlock").is_none()))
        && s.prop("BranchType").is_none_or(|value| value != "Free")
        && s.sections()
            .filter(|b| b.tag == "Branch")
            .all(|b| ordinary(b, false))
}
fn port<'a>(
    ep: &Endpoint,
    names: &'a BTreeMap<BlockId, String>,
    default_kind: PortKind,
) -> Result<Port<'a>, ImportError> {
    Ok(Port {
        name: names
            .get(&ep.block)
            .ok_or_else(|| ImportError::Edit("hierarchy endpoint block missing".into()))?,
        sid: Some(ep.block.0.clone()),
        port: ep.port,
        default_kind,
    })
}
fn rewrite(
    original: &Section,
    recipe: &LineRecipe,
    original_sys: &Section,
    names: &BTreeMap<BlockId, String>,
) -> Result<Section, ImportError> {
    let mut line = original.clone();
    let LineRecipe::Rewrite {
        keep_destinations,
        source,
        append_destinations,
        clear_points: clear,
        ..
    } = recipe
    else {
        return Ok(line);
    };
    if !ordinary(&line, true) {
        return Err(ImportError::Edit(
            "grouping physical or detached serialized branches is unsupported".into(),
        ));
    }
    let keep_names: BTreeSet<_> = keep_destinations
        .iter()
        .map(|ep| {
            Ok((
                names
                    .get(&ep.block)
                    .ok_or_else(|| ImportError::Edit("hierarchy endpoint block missing".into()))?
                    .clone(),
                ep.port,
            ))
        })
        .collect::<Result<_, ImportError>>()?;
    let keep_sids: BTreeSet<_> = keep_destinations
        .iter()
        .map(|ep| (ep.block.0.clone(), ep.port))
        .collect();
    prune(
        &mut line,
        &|section, _| {
            if let Some(value) = section.prop("Dst") {
                return !parse_endpoint(&value, PortKind::In)
                    .is_some_and(|(sid, p)| keep_sids.contains(&(sid.to_string(), p)));
            }
            !section
                .prop("DstBlock")
                .zip(parse_port(
                    section.prop("DstPort").as_deref().unwrap_or("1"),
                    PortKind::In,
                ))
                .is_some_and(|endpoint| keep_names.contains(&endpoint))
        },
        true,
    );
    for key in SRC_FORMS {
        line.remove_prop(key);
    }
    let src = port(source, names, PortKind::Out)?;
    src.write(&mut line, SRC_FORMS);
    if *clear {
        clear_points(&mut line);
    }
    let mut temporary = new_section("System", "", original_sys.eol());
    temporary.items.push(Item::Section(line));
    for destination in append_destinations {
        connect(
            &mut temporary,
            &src,
            &port(destination, names, PortKind::In)?,
        );
    }
    let rewritten = temporary
        .sections()
        .find(|s| s.tag == "Line")
        .unwrap()
        .clone();
    Ok(rewritten)
}

/// Persistent IDs assigned to legacy blocks must update both endpoint spellings.
pub(super) fn remap_sids(line: &mut Section, map: &[(BlockId, BlockId)]) {
    for (key, kind) in [("Src", PortKind::Out), ("Dst", PortKind::In)] {
        if let Some(value) = line.prop(key) {
            if let Some((sid, port)) = parse_endpoint(&value, kind) {
                if let Some((_, new)) = map.iter().find(|(old, _)| old.0 == sid) {
                    line.set_prop(
                        key,
                        &Endpoint {
                            block: new.clone(),
                            port,
                        }
                        .to_string(),
                        false,
                    );
                }
            }
        }
    }
    for item in &mut line.items {
        if let Item::Section(branch) = item {
            if branch.tag == "Branch" {
                remap_sids(branch, map);
            }
        }
    }
}

pub(super) fn create(
    sys: &mut Section,
    resolved: &Resolved,
    plan: &CreatePlan,
) -> Result<(), ImportError> {
    let raw_lines: Vec<_> = sys
        .sections()
        .filter(|s| s.tag == "Line")
        .cloned()
        .collect();
    if raw_lines.len() != resolved.source_line_count {
        return Err(ImportError::Edit(
            "serialized line roots differ from imported hierarchy".into(),
        ));
    }
    let mut names: BTreeMap<BlockId, String> = resolved
        .names
        .iter()
        .map(|(id, name)| (id.clone(), name.clone()))
        .collect();
    names.insert(plan.wrapper.id.clone(), plan.wrapper.name.clone());
    let child = plan
        .wrapper
        .subsystem
        .as_deref()
        .ok_or_else(|| ImportError::Edit("new subsystem missing".into()))?;
    names.extend(child.blocks.iter().map(|b| (b.id.clone(), b.name.clone())));
    let build = |recipe: &LineRecipe| {
        let raw = raw_lines
            .get(recipe_index(recipe))
            .ok_or_else(|| ImportError::Edit("serialized hierarchy line missing".into()))?;
        rewrite(raw, recipe, sys, &names)
    };
    let mut parent_lines: BTreeMap<usize, Section> = plan
        .parent_lines
        .iter()
        .map(|r| Ok((recipe_index(r), build(r)?)))
        .collect::<Result<_, ImportError>>()?;
    let mut child_lines: Vec<_> = plan
        .child_lines
        .iter()
        .map(build)
        .collect::<Result<_, _>>()?;
    for line in &mut child_lines {
        remap_sids(line, &plan.id_remap);
    }
    let indent = child_indent(sys);
    let mut inner = new_section("System", &format!("{indent}  "), sys.eol());
    let selected: BTreeSet<_> = plan.selected_indices.iter().copied().collect();
    let (mut block_index, mut line_index) = (0, 0);
    let mut retained = Vec::new();
    for item in std::mem::take(&mut sys.items) {
        match item {
            Item::Section(mut block) if block.tag == "Block" => {
                let is_selected = selected.contains(&block_index);
                block_index += 1;
                if is_selected {
                    // A legacy MDL path ID becomes a persistent numeric SID.
                    if let Some(moved) = child
                        .blocks
                        .iter()
                        .find(|b| Some(b.name.as_str()) == block.prop("Name").as_deref())
                    {
                        if block.prop("SID").as_deref() != Some(moved.id.0.as_str()) {
                            block.set_prop("SID", &moved.id.0, false);
                        }
                    }
                    inner.items.push(Item::Section(block));
                } else {
                    retained.push(Item::Section(block));
                }
            }
            Item::Section(line) if line.tag == "Line" => {
                if let Some(replacement) = parent_lines.remove(&line_index) {
                    retained.push(Item::Section(replacement));
                }
                line_index += 1;
            }
            other => retained.push(other),
        }
    }
    sys.items = retained;
    for block in &plan.generated_ports {
        add_block(&mut inner, block);
    }
    inner
        .items
        .extend(child_lines.into_iter().map(Item::Section));
    add_block(sys, &plan.wrapper);
    let wrapper = sys
        .sections_mut()
        .find(|b| b.tag == "Block" && b.prop("SID").as_deref() == Some(plan.wrapper.id.0.as_str()))
        .ok_or_else(|| ImportError::Edit("new raw subsystem missing".into()))?;
    wrapper.items.push(Item::Section(inner));
    Ok(())
}
