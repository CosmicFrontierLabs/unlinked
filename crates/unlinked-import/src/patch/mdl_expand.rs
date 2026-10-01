//! Collapse ordinary subsystem boundaries by grafting raw destination forests.
use super::*;
use unlinked_model::expand::{ExpandPlan, RootRef};

fn fail(message: &str) -> ImportError {
    ImportError::Edit(message.into())
}
fn endpoints(key: &str) -> bool {
    SRC_FORMS.contains(&key) || DST_FORMS.contains(&key) || matches!(key, "Points" | "ZOrder")
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
fn target_paths(
    node: &Section,
    target: &Port<'_>,
    path: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    if target.is_at(node, DST_FORMS) {
        out.push(path.clone());
    }
    for (i, branch) in node.sections().filter(|b| b.tag == "Branch").enumerate() {
        path.push(i);
        target_paths(branch, target, path, out);
        path.pop();
    }
}
fn graft_at(node: &mut Section, path: &[usize], donor: &Section) {
    if let Some((&i, rest)) = path.split_first() {
        let child = node
            .items
            .iter_mut()
            .filter_map(|item| match item {
                Item::Section(b) if b.tag == "Branch" => Some(b),
                _ => None,
            })
            .nth(i)
            .expect("original branch path retained");
        graft_at(child, rest, donor);
        return;
    }
    for key in DST_FORMS {
        node.remove_prop(key);
    }
    if donor.prop("Dst").is_some() || donor.prop("DstBlock").is_some() {
        let mut branch = new_section(
            "Branch",
            &format!("{}  ", indent_of(&node.header)),
            node.eol(),
        );
        branch.items.extend(
            donor
                .items
                .iter()
                .filter(
                    |item| matches!(item,Item::Prop{key,..} if DST_FORMS.contains(&key.as_str())),
                )
                .cloned(),
        );
        node.items.push(Item::Section(branch));
    }
    node.items.extend(
        donor
            .items
            .iter()
            .filter(|item| matches!(item,Item::Section(b) if b.tag=="Branch"))
            .cloned(),
    );
}
fn removable(
    s: &Section,
    system: bool,
    allowed: &std::collections::BTreeMap<String, String>,
) -> Result<(), ImportError> {
    if system && s.sections().filter(|s| s.tag == "System").count() != 1 {
        return Err(fail("ambiguous raw child systems"));
    }
    for item in &s.items {
        match item {
            Item::Section(child) if system && child.tag == "System" => {}
            Item::Prop { key, lines }
                if super::expansion_property(key)
                    || allowed
                        .get(key)
                        .is_some_and(|value| *value == decode(lines)) => {}
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
    removable(&wrapper, true, &plan.wrapper_parameters)?;
    let child = wrapper
        .sections()
        .find(|s| s.tag == "System")
        .ok_or_else(|| fail("raw child system missing"))?;
    if child.items.iter().any(|i| {
        !matches!(i,Item::Section(s) if s.tag=="Block" || s.tag=="Line")
            && !matches!(i,Item::Raw(s) if s.trim().is_empty())
            && !matches!(i,Item::Prop{key,..} if unlinked_model::expand::view_property(key))
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
            removable(b, false, &plan.port_parameters[&index])?;
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
        // Resolve every target against the original scope before any donor
        // introduces identically named blocks from the other scope.
        let mut targets = Vec::new();
        for g in &recipe.grafts {
            super::super::expansion::charge(&mut budget, weight(&line), 1)?;
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
            let mut paths = Vec::new();
            target_paths(&line, &target, &mut Vec::new(), &mut paths);
            if paths.len() != 1 {
                return Err(fail("serialized boundary destination is ambiguous"));
            }
            targets.push(paths.pop().unwrap());
        }
        for (g, path) in recipe.grafts.iter().zip(targets) {
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
            graft_at(&mut line, &path, donor);
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
