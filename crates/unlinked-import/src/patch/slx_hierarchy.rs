//! Move existing XML nodes into an inline virtual subsystem.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::hierarchy::{CreatePlan, LineRecipe};

fn recipe_index(recipe: &LineRecipe) -> usize {
    match recipe {
        LineRecipe::Keep { original_index } | LineRecipe::Rewrite { original_index, .. } => {
            *original_index
        }
    }
}
fn ordinary(e: &XElem, root: bool) -> bool {
    (root || e.prop("Src").is_none())
        && e.prop("BranchType").is_none_or(|value| value != "Free")
        && e.elements()
            .filter(|b| b.name == "Branch")
            .all(|b| ordinary(b, false))
}
fn rewrite(original: &XElem, recipe: &LineRecipe) -> Result<XElem, ImportError> {
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
    let keep: BTreeSet<_> = keep_destinations
        .iter()
        .map(|ep| (ep.block.0.clone(), ep.port))
        .collect();
    prune(
        &mut line,
        &|value| {
            !parse_endpoint(value, PortKind::In)
                .is_some_and(|(sid, p)| keep.contains(&(sid.to_string(), p)))
        },
        true,
    );
    match line.prop_mut("Src") {
        Some(p) => p.set_text(&source.to_string()),
        None => line.push_prop("Src", &source.to_string()),
    }
    if *clear {
        clear_points(&mut line);
    }
    let mut temporary = XElem::new("System");
    temporary.children.push(XNode::Element(line));
    for destination in append_destinations {
        connect(&mut temporary, source, destination);
    }
    let rewritten = temporary
        .elements()
        .find(|e| e.name == "Line")
        .unwrap()
        .clone();
    Ok(rewritten)
}

pub(super) fn create(
    sys: &mut XElem,
    resolved: &Resolved,
    plan: &CreatePlan,
) -> Result<(), ImportError> {
    let raw_lines: Vec<_> = sys
        .elements()
        .filter(|e| e.name == "Line")
        .cloned()
        .collect();
    if raw_lines.len() != resolved.source_line_count {
        return Err(ImportError::Edit(
            "serialized line roots differ from imported hierarchy".into(),
        ));
    }
    let build = |recipe: &LineRecipe| {
        let raw = raw_lines
            .get(recipe_index(recipe))
            .ok_or_else(|| ImportError::Edit("serialized hierarchy line missing".into()))?;
        rewrite(raw, recipe)
    };
    let mut parent_lines: BTreeMap<usize, XElem> = plan
        .parent_lines
        .iter()
        .map(|r| Ok((recipe_index(r), build(r)?)))
        .collect::<Result<_, ImportError>>()?;
    let child_lines: Vec<_> = plan
        .child_lines
        .iter()
        .map(build)
        .collect::<Result<_, _>>()?;
    let indent = indent_in(sys);
    let mut inner = new_element("System", &deeper(&indent));
    let selected: BTreeSet<_> = plan.selected_indices.iter().copied().collect();
    let (mut block_index, mut line_index) = (0, 0);
    let mut retained = Vec::new();
    for node in std::mem::take(&mut sys.children) {
        match node {
            XNode::Element(block) if block.name == "Block" => {
                let is_selected = selected.contains(&block_index);
                block_index += 1;
                if is_selected {
                    // SLX block SIDs are already persistent; never invent one
                    // for an opaque record that lacked a resolvable identity.
                    if block.attr("SID").is_none() {
                        return Err(ImportError::Edit(
                            "grouping SLX blocks without SIDs is unsupported".into(),
                        ));
                    }
                    let at = end_of(&inner);
                    insert_child(&mut inner, at, block, &deeper(&indent));
                } else {
                    retained.push(XNode::Element(block));
                }
            }
            XNode::Element(line) if line.name == "Line" => {
                if let Some(replacement) = parent_lines.remove(&line_index) {
                    retained.push(XNode::Element(replacement));
                }
                line_index += 1;
            }
            other => retained.push(other),
        }
    }
    sys.children = retained;
    for block in &plan.generated_ports {
        add_block(&mut inner, block);
    }
    for line in child_lines {
        let at = end_of(&inner);
        insert_child(&mut inner, at, line, &deeper(&indent));
    }
    add_block(sys, &plan.wrapper);
    let wrapper = sys
        .elements_mut()
        .find(|b| b.name == "Block" && b.attr("SID").as_deref() == Some(plan.wrapper.id.0.as_str()))
        .ok_or_else(|| ImportError::Edit("new raw subsystem missing".into()))?;
    let at = end_of(wrapper);
    insert_child(wrapper, at, inner, &deeper(&indent));
    Ok(())
}
