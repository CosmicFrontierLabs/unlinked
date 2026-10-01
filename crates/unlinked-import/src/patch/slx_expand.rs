//! Raw-tree expansion of inline ordinary subsystems.
use super::*;
use unlinked_model::expand::{ExpandPlan, RootRef};
fn fail(message: &str) -> ImportError {
    ImportError::Edit(message.into())
}
fn endpoint_property(e: &XElem) -> bool {
    e.name == "P"
        && e.attr("Name")
            .is_some_and(|k| matches!(k.as_str(), "Src" | "Dst" | "Points" | "ZOrder"))
}
fn metadata(e: &XElem) -> Vec<XNode> {
    e.children
        .iter()
        .filter(|c| match c {
            XNode::Element(p) => p.name != "Branch" && !endpoint_property(p),
            XNode::Text(s) => !s.trim().is_empty(),
            XNode::Raw(_) => true,
        })
        .cloned()
        .collect()
}
fn ordinary(e: &XElem, root: bool) -> bool {
    (root || e.prop("Src").is_none())
        && e.prop("BranchType").is_none_or(|v| v != "Free")
        && e.elements()
            .filter(|b| b.name == "Branch")
            .all(|b| ordinary(b, false))
}
fn weight(e: &XElem) -> usize {
    64 + e.name.len()
        + e.attrs
            .iter()
            .map(|(k, v)| 64 + k.len() + v.len())
            .sum::<usize>()
        + e.children
            .iter()
            .map(|c| match c {
                XNode::Element(e) => weight(e),
                XNode::Text(s) | XNode::Raw(s) => 64 + s.len(),
            })
            .sum::<usize>()
}
fn graft(node: &mut XElem, target: &str, donor: &XElem) -> usize {
    let mut count = 0;
    for b in node.elements_mut().filter(|b| b.name == "Branch") {
        count += graft(b, target, donor);
    }
    if node.prop("Dst").as_deref() == Some(target) {
        count += 1;
        node.children.retain(|c|!matches!(c,XNode::Element(p) if p.name=="P" && p.attr("Name").as_deref()==Some("Dst")));
        if let Some(dst)=donor.children.iter().find(|c|matches!(c,XNode::Element(p) if p.name=="P" && p.attr("Name").as_deref()==Some("Dst"))) {let mut b=XElem::new("Branch");b.children.push(dst.clone());node.children.push(XNode::Element(b));}
        node.children.extend(
            donor
                .children
                .iter()
                .filter(|c| matches!(c,XNode::Element(b) if b.name=="Branch"))
                .cloned(),
        );
    }
    count
}
fn removable(
    e: &XElem,
    system: bool,
    allowed: &std::collections::BTreeMap<String, String>,
) -> Result<(), ImportError> {
    if system && e.elements().filter(|s| s.name == "System").count() != 1 {
        return Err(fail("ambiguous raw child systems"));
    }
    if e.attrs
        .iter()
        .any(|(k, _)| !matches!(k.as_str(), "BlockType" | "Name" | "SID"))
    {
        return Err(fail("expansion would discard opaque block attributes"));
    }
    for c in &e.children {
        match c {
            XNode::Element(p)
                if p.name == "P"
                    && p.attr("Name").is_some_and(|k| {
                        super::expansion_property(&k)
                            || allowed.get(&k).is_some_and(|v| p.text() == *v)
                    }) => {}
            XNode::Element(s) if system && s.name == "System" => {}
            XNode::Text(s) if s.trim().is_empty() => {}
            _ => {
                return Err(fail(
                    "expansion would discard opaque wrapper or port metadata",
                ))
            }
        }
    }
    Ok(())
}
pub(super) fn expand(parent: &mut XElem, plan: &ExpandPlan) -> Result<(), ImportError> {
    let wrapper = parent
        .elements()
        .filter(|b| b.name == "Block")
        .nth(plan.wrapper_index)
        .ok_or_else(|| fail("raw subsystem missing"))?
        .clone();
    removable(&wrapper, true, &plan.wrapper_parameters)?;
    let child = wrapper
        .elements()
        .find(|e| e.name == "System")
        .ok_or_else(|| fail("raw child system missing"))?;
    if !child.attrs.is_empty() {
        return Err(fail(
            "expanding referenced or attributed child systems is not supported yet",
        ));
    }
    if child.children.iter().any(|c| {
        !matches!(c,XNode::Element(e) if e.name=="Block"||e.name=="Line")
            && !matches!(c,XNode::Text(s) if s.trim().is_empty())
            && !matches!(c,XNode::Element(p) if p.name=="P" && p.attr("Name").is_some_and(|k|unlinked_model::expand::view_property(&k)))
    }) {
        return Err(fail("expansion would discard child system metadata"));
    }
    let parent_lines: Vec<_> = parent
        .elements()
        .filter(|e| e.name == "Line")
        .cloned()
        .collect();
    let child_lines: Vec<_> = child
        .elements()
        .filter(|e| e.name == "Line")
        .cloned()
        .collect();
    if parent_lines.len() != plan.parent_line_count || child_lines.len() != plan.child_line_count {
        return Err(fail("serialized expansion line roots differ from model"));
    }
    let children: Vec<_> = child.elements().filter(|e| e.name == "Block").collect();
    for (index, b) in children.iter().enumerate() {
        if !plan.moved_indices.contains(&index) {
            removable(b, false, &plan.port_parameters[&index])?;
        }
    }
    let raw = |r: RootRef| -> &XElem {
        match r {
            RootRef::Parent(i) => &parent_lines[i],
            RootRef::Child(i) => &child_lines[i],
        }
    };
    let mut lines = Vec::new();
    let mut budget = 64 * 1024 * 1024;
    for recipe in &plan.lines {
        super::super::expansion::charge(&mut budget, weight(raw(recipe.base)), 1)?;
        let mut line = raw(recipe.base).clone();
        if !recipe.grafts.is_empty() && !ordinary(&line, true) {
            return Err(fail("physical expansion net unsupported"));
        }
        let base_metadata = metadata(&line);
        for g in &recipe.grafts {
            let donor = raw(g.donor);
            super::super::expansion::charge(
                &mut budget,
                weight(&line) + weight(donor),
                donor.children.len() + 1,
            )?;
            if donor.attrs.iter().any(|a| !line.attrs.contains(a))
                || !ordinary(donor, true)
                || !super::super::expansion::subset(&metadata(donor), &base_metadata)
            {
                return Err(fail("incompatible boundary line metadata"));
            }
            if graft(&mut line, &g.destination.to_string(), donor) != 1 {
                return Err(fail("serialized boundary destination is ambiguous"));
            }
        }
        if recipe.clear_points {
            clear_points(&mut line);
        }
        lines.push(line);
    }
    let mut moved = Vec::new();
    for (&index, final_block) in plan.moved_indices.iter().zip(&plan.moved_blocks) {
        let mut block = children[index].clone();
        block.set_attr("SID", &final_block.id.0);
        set_parameter(&mut block, "Position", &format_rect(&final_block.position));
        moved.push(block);
    }
    let mut block_index = 0;
    let mut moved = Some(moved);
    let mut kept = Vec::new();
    for node in std::mem::take(&mut parent.children) {
        match &node {
            XNode::Element(b) if b.name == "Block" => {
                if block_index == plan.wrapper_index {
                    kept.extend(moved.take().unwrap().into_iter().map(XNode::Element));
                } else {
                    kept.push(node);
                }
                block_index += 1;
            }
            XNode::Element(l) if l.name == "Line" => {}
            _ => kept.push(node),
        }
    }
    parent.children = kept;
    let indent = indent_in(parent);
    for line in lines {
        let at = end_of(parent);
        insert_child(parent, at, line, &indent);
    }
    Ok(())
}
