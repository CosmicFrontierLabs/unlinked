//! Collapse ordinary subsystem boundaries by grafting raw destination forests.
use super::*;
use unlinked_model::expand::{ExpandPlan, RootRef};

fn fail(message: &str) -> ImportError {
    ImportError::Edit(message.into())
}
fn endpoints(key: &str) -> bool {
    SRC_FORMS.contains(&key) || DST_FORMS.contains(&key) || key == "Points"
}
fn metadata(s: &Section) -> Vec<(String, String)> {
    s.items
        .iter()
        .filter_map(|item| match item {
            Item::Prop { key, lines } if !endpoints(key) => Some((key.clone(), decode(lines))),
            Item::Section(c) if c.tag != "Branch" => {
                let mut raw = String::new();
                c.write(&mut raw);
                Some(("opaque".into(), raw))
            }
            Item::Raw(raw) if !raw.trim().is_empty() => Some(("raw".into(), raw.clone())),
            _ => None,
        })
        .collect()
}
fn weight(s: &Section) -> usize {
    s.header.len()
        + s.footer.len()
        + 64
        + s.items
            .iter()
            .map(|i| match i {
                Item::Prop { key, lines } => {
                    64 + key.len() + lines.iter().map(String::len).sum::<usize>()
                }
                Item::Raw(s) => 64 + s.len(),
                Item::Section(s) => weight(s),
            })
            .sum::<usize>()
}
fn graft(node: &mut Section, target: &Port<'_>, donor: &Section) -> usize {
    let mut count = 0;
    for item in &mut node.items {
        if let Item::Section(b) = item {
            if b.tag == "Branch" {
                count += graft(b, target, donor);
            }
        }
    }
    if target.is_at(node, DST_FORMS) {
        count += 1;
        for key in DST_FORMS {
            node.remove_prop(key);
        }
        if donor.prop("Dst").is_some() || donor.prop("DstBlock").is_some() {
            let mut b = new_section(
                "Branch",
                &format!("{}  ", indent_of(&node.header)),
                node.eol(),
            );
            for item in &donor.items {
                if let Item::Prop { key, .. } = item {
                    if DST_FORMS.contains(&key.as_str()) {
                        b.items.push(item.clone());
                    }
                }
            }
            node.items.push(Item::Section(b));
        }
        node.items.extend(
            donor
                .items
                .iter()
                .filter(|item| matches!(item,Item::Section(b) if b.tag=="Branch"))
                .cloned(),
        );
    }
    count
}
fn removable(s: &Section, system: bool) -> Result<(), ImportError> {
    if system && s.sections().filter(|s| s.tag == "System").count() != 1 {
        return Err(fail("ambiguous raw child systems"));
    }
    for item in &s.items {
        match item {
            Item::Section(child) if system && child.tag == "System" => {}
            Item::Prop { key, .. } if super::expansion_property(key) => {}
            Item::Raw(raw) if raw.trim().is_empty() => {}
            _ => {
                return Err(fail(
                    "expansion would discard opaque wrapper or port metadata",
                ))
            }
        }
    }
    Ok(())
}
fn ordinary(s: &Section, root: bool) -> bool {
    (root || (s.prop("Src").is_none() && s.prop("SrcBlock").is_none()))
        && s.prop("BranchType").is_none_or(|v| v != "Free")
        && s.sections()
            .filter(|b| b.tag == "Branch")
            .all(|b| ordinary(b, false))
}
pub(super) fn expand(
    parent: &mut Section,
    resolved: &Resolved,
    plan: &ExpandPlan,
) -> Result<(), ImportError> {
    let wrapper = parent
        .sections()
        .filter(|s| s.tag == "Block")
        .nth(plan.wrapper_index)
        .ok_or_else(|| fail("raw subsystem missing"))?
        .clone();
    removable(&wrapper, true)?;
    let child = wrapper
        .sections()
        .find(|s| s.tag == "System")
        .ok_or_else(|| fail("raw child system missing"))?;
    if child.items.iter().any(|i| {
        !matches!(i,Item::Section(s) if s.tag=="Block" || s.tag=="Line")
            && !matches!(i,Item::Raw(s) if s.trim().is_empty())
            && !matches!(i,Item::Prop{key,..} if key=="Name")
    }) {
        return Err(fail("expansion would discard child system metadata"));
    }
    let parent_lines: Vec<_> = parent
        .sections()
        .filter(|s| s.tag == "Line")
        .cloned()
        .collect();
    let child_lines: Vec<_> = child
        .sections()
        .filter(|s| s.tag == "Line")
        .cloned()
        .collect();
    if parent_lines.len() != plan.parent_line_count || child_lines.len() != plan.child_line_count {
        return Err(fail("serialized expansion line roots differ from model"));
    }
    let children: Vec<_> = child.sections().filter(|s| s.tag == "Block").collect();
    for (index, b) in children.iter().enumerate() {
        if !plan.moved_indices.contains(&index) {
            removable(b, false)?;
        }
    }
    let raw = |r: RootRef| -> &Section {
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
                donor.items.len() + 1,
            )?;
            if !ordinary(donor, true)
                || !super::super::expansion::subset(&metadata(donor), &base_metadata)
            {
                return Err(fail("incompatible boundary line metadata"));
            }
            let name = match recipe.base {
                RootRef::Parent(_) => resolved.names.get(&g.destination.block),
                RootRef::Child(_) => plan.child_names.get(&g.destination.block),
            }
            .ok_or_else(|| fail("boundary block name missing"))?;
            let target = Port {
                name,
                sid: Some(g.destination.block.0.clone()),
                port: g.destination.port,
                default_kind: PortKind::In,
            };
            if graft(&mut line, &target, donor) != 1 {
                return Err(fail("serialized boundary destination is ambiguous"));
            }
        }
        if recipe.clear_points {
            clear_points(&mut line);
        }
        super::hierarchy::remap_sids(&mut line, &plan.id_remap);
        lines.push(line);
    }
    let mut moved = Vec::new();
    for (&index, final_block) in plan.moved_indices.iter().zip(&plan.moved_blocks) {
        let mut raw = children[index].clone();
        raw.set_prop("SID", &final_block.id.0, false);
        raw.set_prop("Position", &format_rect(&final_block.position), true);
        moved.push(raw);
    }
    let mut block_index = 0;
    let mut moved = Some(moved);
    let mut kept = Vec::new();
    for item in std::mem::take(&mut parent.items) {
        match &item {
            Item::Section(b) if b.tag == "Block" => {
                if block_index == plan.wrapper_index {
                    kept.extend(moved.take().unwrap().into_iter().map(Item::Section));
                } else {
                    kept.push(item);
                }
                block_index += 1;
            }
            Item::Section(l) if l.tag == "Line" => {}
            _ => kept.push(item),
        }
    }
    kept.extend(lines.into_iter().map(Item::Section));
    parent.items = kept;
    Ok(())
}
