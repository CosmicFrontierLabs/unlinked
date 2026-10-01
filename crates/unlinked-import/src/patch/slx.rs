//! Apply edits to an SLX package. The system an edit targets is resolved by
//! its path of block names from the root, following `<System Ref=...>`
//! placeholders into split `systems/*.xml` parts, and the block by SID
//! within that system only. Only parts that change are rewritten; every
//! other zip entry is copied raw.

use super::dom::{self, Document, XElem, XNode};
use super::{format_ports, parse_endpoint};
use super::{Boundary, Resolved};
use crate::{ImportError, MAX_DEPTH, MAX_UNCOMPRESSED_BYTES};
use std::io::{Cursor, Read, Write};
use unlinked_model::edit::{Edit, SID_WATERMARK};
use unlinked_model::geometry::to_rotation;
use unlinked_model::{Block, Endpoint, PortCounts, PortKind};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const ROOT_PART: &str = "simulink/blockdiagram.xml";

fn is_block_part(name: &str) -> bool {
    name == ROOT_PART || (name.starts_with("simulink/systems/") && name.ends_with(".xml"))
}

fn element_mut(parent: &mut XElem, i: usize) -> &mut XElem {
    match &mut parent.children[i] {
        XNode::Element(e) => e,
        _ => unreachable!("indices always refer to elements"),
    }
}

fn element(parent: &XElem, i: usize) -> &XElem {
    match &parent.children[i] {
        XNode::Element(e) => e,
        _ => unreachable!("indices always refer to elements"),
    }
}

/// Index of the only child element matching `pred`; an error names `what`
/// when there is none or more than one.
fn only_child(e: &XElem, what: &str, pred: impl Fn(&XElem) -> bool) -> Result<usize, ImportError> {
    let mut found = e
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, XNode::Element(x) if pred(x)))
        .map(|(i, _)| i);
    match (found.next(), found.next()) {
        (Some(i), None) => Ok(i),
        (None, _) => Err(ImportError::Edit(format!("{what} not found"))),
        (Some(_), Some(_)) => Err(ImportError::Edit(format!("{what} is ambiguous"))),
    }
}

/// A system element: the part holding it and the child-index path from
/// that part's document element.
struct SystemAt {
    part: usize,
    path: Vec<usize>,
}

fn descend<'a>(mut e: &'a XElem, path: &[usize]) -> &'a XElem {
    for &i in path {
        e = element(e, i);
    }
    e
}

fn descend_mut<'a>(mut e: &'a mut XElem, path: &[usize]) -> &'a mut XElem {
    for &i in path {
        e = element_mut(e, i);
    }
    e
}

fn part_root(parts: &[(String, Document, bool)], part: usize) -> Result<&XElem, ImportError> {
    parts[part]
        .1
        .root()
        .ok_or_else(|| ImportError::Xml(format!("{}: empty document", parts[part].0)))
}

/// If the `<System>` at `at` is a `Ref` placeholder, the split part it
/// names; otherwise `at` itself.
fn follow_ref(parts: &[(String, Document, bool)], at: SystemAt) -> Result<SystemAt, ImportError> {
    let Some(r) = descend(part_root(parts, at.part)?, &at.path).attr("Ref") else {
        return Ok(at);
    };
    let part_name = format!("simulink/systems/{r}.xml");
    let part = parts
        .iter()
        .position(|(n, ..)| *n == part_name)
        .ok_or_else(|| ImportError::Xml(format!("{part_name} missing")))?;
    if part_root(parts, part)?.name != "System" {
        return Err(ImportError::Xml(format!("{part_name}: expected <System>")));
    }
    Ok(SystemAt {
        part,
        path: Vec::new(),
    })
}

/// Resolve the system at `path` (block names from the root).
fn locate(parts: &[(String, Document, bool)], path: &[String]) -> Result<SystemAt, ImportError> {
    if path.len() > MAX_DEPTH {
        return Err(ImportError::Edit("system path too deep".into()));
    }
    let part = parts
        .iter()
        .position(|(n, ..)| n == ROOT_PART)
        .ok_or_else(|| ImportError::Xml(format!("{ROOT_PART} missing")))?;
    let root = part_root(parts, part)?;
    let model = only_child(root, "<Model>", |c| {
        matches!(c.name.as_str(), "Model" | "Library" | "Subsystem")
    })?;
    let sys = only_child(element(root, model), "root <System>", |c| {
        c.name == "System"
    })?;
    let mut at = follow_ref(
        parts,
        SystemAt {
            part,
            path: vec![model, sys],
        },
    )?;
    for name in path {
        let sys = descend(part_root(parts, at.part)?, &at.path);
        let b = only_child(sys, &format!("block {name:?}"), |c| {
            c.name == "Block" && c.attr("Name").as_deref() == Some(name.as_str())
        })?;
        let s = only_child(element(sys, b), &format!("system of {name:?}"), |c| {
            c.name == "System"
        })?;
        at.path.extend([b, s]);
        at = follow_ref(parts, at)?;
    }
    Ok(at)
}

/// The system element at `at`, marking its part changed.
fn system_at<'a>(
    parts: &'a mut [(String, Document, bool)],
    at: &SystemAt,
) -> Option<&'a mut XElem> {
    let (_, doc, changed) = &mut parts[at.part];
    *changed = true;
    Some(descend_mut(doc.root_mut()?, &at.path))
}

fn refers_to(e: &XElem, key: &str, sid: &str) -> bool {
    e.prop(key)
        .is_some_and(|v| v.split_once('#').is_some_and(|(b, _)| b == sid))
}

fn touches(line: &XElem, sid: &str) -> bool {
    refers_to(line, "Src", sid)
        || refers_to(line, "Dst", sid)
        || line
            .elements()
            .any(|b| b.name == "Branch" && touches(b, sid))
}

fn clear_points(e: &mut XElem) {
    e.children
        .retain(|c| !matches!(c, XNode::Element(p) if p.name == "P" && p.attr("Name").as_deref() == Some("Points")));
    for b in e.elements_mut().filter(|b| b.name == "Branch") {
        clear_points(b);
    }
}

/// Remove endpoints whose value `hit` matches from a line or branch;
/// returns whether it still leads anywhere. Physical-connection branches
/// store their port as `Src`, so both keys count as destinations below the
/// line's own source.
fn prune(e: &mut XElem, hit: &dyn Fn(&str) -> bool, is_line: bool) -> bool {
    e.children.retain_mut(|c| match c {
        XNode::Element(p) if p.name == "P" => {
            let name = p.attr("Name");
            let dst =
                name.as_deref() == Some("Dst") || (!is_line && name.as_deref() == Some("Src"));
            !(dst && hit(&p.text()))
        }
        XNode::Element(b) if b.name == "Branch" => prune(b, hit, false),
        _ => true,
    });
    e.prop("Dst").is_some()
        || (!is_line && e.prop("Src").is_some())
        || e.elements().any(|b| b.name == "Branch")
}

fn refers_to_value(v: &str, sid: &str) -> bool {
    v.split_once('#').is_some_and(|(b, _)| b == sid)
}

fn format_rect(p: &unlinked_model::Rect) -> String {
    let n = |v: f64| {
        if v.fract() == 0.0 {
            format!("{}", v as i64)
        } else {
            format!("{v}")
        }
    };
    format!(
        "[{}, {}, {}, {}]",
        n(p.left),
        n(p.top),
        n(p.right),
        n(p.bottom)
    )
}

/// Set a mask parameter's value in either the `<Mask>` element form or the
/// older `Simulink.MaskParameter` object form. Returns whether one matched.
fn set_mask_parameter(block: &mut XElem, name: &str, value: &str) -> bool {
    fn visit(e: &mut XElem, name: &str, value: &str) -> bool {
        if e.name == "MaskParameter" && e.attr("Name").as_deref() == Some(name) {
            let existing = e
                .children
                .iter()
                .position(|c| matches!(c, XNode::Element(v) if v.name == "Value"));
            match existing {
                Some(i) => element_mut(e, i).set_text(value),
                None => {
                    let mut v = XElem::new("Value");
                    v.set_text(value);
                    e.children.push(XNode::Element(v));
                }
            }
            return true;
        }
        if e.attr("ClassName").as_deref() == Some("Simulink.MaskParameter")
            && e.prop("Name").as_deref() == Some(name)
        {
            match e.prop_mut("Value") {
                Some(v) => v.set_text(value),
                None => e.push_prop("Value", value),
            }
            return true;
        }
        e.elements_mut().any(|c| visit(c, name, value))
    }
    block
        .elements_mut()
        .filter(|c| c.name == "Mask" || c.attr("PropName").as_deref() == Some("MaskObject"))
        .any(|m| visit(m, name, value))
}

fn set_parameter(block: &mut XElem, name: &str, value: &str) {
    if set_mask_parameter(block, name, value) {
        return;
    }
    if let Some(p) = block.prop_mut(name) {
        p.set_text(value);
        return;
    }
    if let Some(inst) = block.elements_mut().find(|e| e.name == "InstanceData") {
        if let Some(p) = inst.prop_mut(name) {
            p.set_text(value);
            return;
        }
    }
    block.push_prop(name, value);
}

/// Apply an edit to the system element `parent`.
fn apply_edit(parent: &mut XElem, resolved: &Resolved) -> Result<(), ImportError> {
    let edit = &resolved.edit;
    if let Edit::AddAnnotation {
        id, text, position, ..
    } = edit
    {
        let indent = indent_in(parent);
        let mut a = new_element("Annotation", &indent);
        a.set_attr("SID", id);
        a.push_prop("Name", text);
        a.push_prop("Position", &format_rect(position));
        let at = after_last(parent, &["Annotation", "Block", "Line", "P"]);
        insert_child(parent, at, a, &indent);
        return Ok(());
    }
    let annotation = match edit {
        Edit::MoveAnnotation { target, .. }
        | Edit::SetAnnotationText { target, .. }
        | Edit::DeleteAnnotation { target, .. } => Some(target.index),
        _ => None,
    };
    if let Some(index) = annotation {
        let at = parent
            .children
            .iter()
            .enumerate()
            .filter_map(|(i, node)| {
                matches!(node, XNode::Element(a) if a.name == "Annotation").then_some(i)
            })
            .nth(index)
            .ok_or_else(|| ImportError::Edit("serialized annotation missing".into()))?;
        if matches!(edit, Edit::DeleteAnnotation { .. }) {
            parent.children.remove(at);
        } else {
            let a = element_mut(parent, at);
            let (key, value) = match edit {
                Edit::MoveAnnotation { position, .. } => ("Position", format_rect(position)),
                Edit::SetAnnotationText { text, .. } => (
                    if a.attr("Text").is_some() || a.prop("Text").is_some() {
                        "Text"
                    } else {
                        "Name"
                    },
                    text.clone(),
                ),
                _ => unreachable!(),
            };
            if a.attr(key).is_some() {
                a.set_attr(key, &value);
            } else {
                match a.prop_mut(key) {
                    Some(p) => p.set_text(&value),
                    None => a.push_prop(key, &value),
                }
            }
        }
        return Ok(());
    }
    let sid = match edit {
        Edit::AddAnnotation { .. }
        | Edit::MoveAnnotation { .. }
        | Edit::SetAnnotationText { .. }
        | Edit::DeleteAnnotation { .. } => unreachable!("applied above"),
        Edit::AddBlock { .. } => {
            let block = resolved
                .added
                .as_ref()
                .ok_or_else(|| ImportError::Edit("the added block is missing".into()))?;
            add_block(parent, block);
            return Ok(());
        }
        Edit::Connect { src, dst, .. } => {
            connect(parent, src, dst);
            return Ok(());
        }
        Edit::Disconnect { dst, .. } => {
            let hit = |v: &str| is_endpoint(v, dst, PortKind::In);
            parent.children.retain_mut(|c| match c {
                XNode::Element(l) if l.name == "Line" && reaches(l, &hit, true) => {
                    prune(l, &hit, true)
                }
                _ => true,
            });
            return Ok(());
        }
        Edit::SetSignalName { src, name, .. } => {
            let i = only_child(parent, "signal source", |l| {
                l.name == "Line"
                    && l.prop("Src")
                        .is_some_and(|v| is_endpoint(&v, src, PortKind::Out))
            })?;
            let line = element_mut(parent, i);
            if name.is_empty() {
                line.children.retain(|c|!matches!(c,XNode::Element(p) if p.name=="P" && p.attr("Name").as_deref()==Some("Name")));
            } else if let Some(p) = line.prop_mut("Name") {
                p.set_text(name);
            } else {
                line.push_prop("Name", name);
            }
            return Ok(());
        }
        Edit::SetRoute { .. } | Edit::SetTrunkRoute { .. } => {
            let update = resolved
                .route
                .as_ref()
                .ok_or_else(|| ImportError::Edit("route update missing".into()))?;
            let i = only_child(parent, "route source", |l| {
                l.name == "Line"
                    && l.prop("Src")
                        .is_some_and(|v| is_endpoint(&v, &update.src, PortKind::Out))
            })?;
            set_route_points(element_mut(parent, i), &update.points)?;
            return Ok(());
        }
        Edit::MoveBlock { id, .. }
        | Edit::SetParameter { id, .. }
        | Edit::RenameBlock { id, .. }
        | Edit::DeleteBlock { id, .. }
        | Edit::SetOrientation { id, .. } => id.0.clone(),
    };
    let i = only_child(parent, &format!("block {sid}"), |c| {
        c.name == "Block" && c.attr("SID").as_deref() == Some(sid.as_str())
    })?;
    match edit {
        Edit::MoveBlock { position, .. } => {
            let block = element_mut(parent, i);
            match block.prop_mut("Position") {
                Some(p) => p.set_text(&format_rect(position)),
                None => block.push_prop("Position", &format_rect(position)),
            }
            for line in parent
                .elements_mut()
                .filter(|l| l.name == "Line" && touches(l, &sid))
            {
                clear_points(line);
            }
        }
        Edit::SetOrientation {
            orientation,
            mirrored,
            ..
        } => {
            let block = element_mut(parent, i);
            let (rotation, mirror) = to_rotation(*orientation, *mirrored);
            block.children.retain(|c| {
                !matches!(c, XNode::Element(p) if p.name == "P" && p.attr("Name").as_deref() == Some("Orientation"))
            });
            for (name, value) in [
                ("BlockRotation", rotation.to_string()),
                ("BlockMirror", if mirror { "on" } else { "off" }.to_string()),
            ] {
                match block.prop_mut(name) {
                    Some(p) => p.set_text(&value),
                    None => block.push_prop(name, &value),
                }
            }
            for line in parent
                .elements_mut()
                .filter(|l| l.name == "Line" && touches(l, &sid))
            {
                clear_points(line);
            }
        }
        Edit::SetParameter { name, value, .. } => {
            let block = element_mut(parent, i);
            set_parameter(block, name, value);
            if let Some(ports) = &resolved.ports {
                set_ports(block, ports);
            }
        }
        Edit::RenameBlock { name, .. } => element_mut(parent, i).set_attr("Name", name),
        Edit::DeleteBlock { .. } => {
            parent.children.remove(i);
            let hit = |v: &str| refers_to_value(v, &sid);
            parent.children.retain_mut(|c| match c {
                // Leave unrelated (possibly already dangling) lines alone.
                XNode::Element(l) if l.name == "Line" && touches(l, &sid) => {
                    !refers_to(l, "Src", &sid) && prune(l, &hit, true)
                }
                _ => true,
            });
        }
        Edit::AddAnnotation { .. }
        | Edit::MoveAnnotation { .. }
        | Edit::SetAnnotationText { .. }
        | Edit::DeleteAnnotation { .. }
        | Edit::AddBlock { .. }
        | Edit::Connect { .. }
        | Edit::Disconnect { .. }
        | Edit::SetRoute { .. }
        | Edit::SetTrunkRoute { .. }
        | Edit::SetSignalName { .. } => {
            unreachable!("applied above")
        }
    }
    // Deleting or renumbering a port block renumbers its siblings.
    for (id, number) in &resolved.renumbered {
        let Some(b) = parent
            .elements_mut()
            .find(|b| b.name == "Block" && b.attr("SID").as_deref() == Some(id.0.as_str()))
        else {
            continue;
        };
        let interface = b
            .elements_mut()
            .find(|l| l.name == "List" && l.attr("ListType").as_deref() == Some("InterfaceData"));
        let bus = interface.is_some();
        if let Some(p) = interface.and_then(|l| l.prop_mut("PortNumber")) {
            p.set_text(number);
        }
        if !bus || b.prop("Port").is_some() {
            set_parameter(b, "Port", number);
        }
    }
    Ok(())
}

/// Update the subsystem block around an edited system, the system element
/// `sys`: its port count and the connections outside it.
fn apply_boundary(sys: &mut XElem, boundary: &Boundary) -> Result<(), ImportError> {
    let remap = &boundary.remap;
    let sid = remap.parent.0.as_str();
    let i = only_child(sys, &format!("block {sid}"), |c| {
        c.name == "Block" && c.attr("SID").as_deref() == Some(sid)
    })?;
    set_ports(element_mut(sys, i), &boundary.ports);
    let (key, default_kind) = match remap.kind {
        PortKind::Out => ("Src", PortKind::Out),
        _ => ("Dst", PortKind::In),
    };
    let index_of = |v: &str| {
        parse_endpoint(v, default_kind)
            .filter(|(b, p)| *b == sid && p.kind == remap.kind && p.index >= 1)
            .map(|(_, p)| p.index)
    };
    let new_index = |old: u32| remap.map.get(old as usize - 1).copied().flatten();
    let gone = |v: &str| index_of(v).is_some_and(|i| new_index(i).is_none());
    // Connections on removed ports go (the IR already refused them unless
    // disconnecting) and the rest move to their new numbers. As in the IR,
    // only what this cuts is removed; other dangling wiring stays.
    fn leads_nowhere(e: &XElem) -> bool {
        e.prop("Dst").is_none() && !e.elements().any(|b| b.name == "Branch")
    }
    fn cut(e: &mut XElem, gone: &dyn Fn(&str) -> bool) -> bool {
        let mut cut_any = false;
        e.children.retain_mut(|c| match c {
            XNode::Element(p) if p.name == "P" && p.attr("Name").as_deref() == Some("Dst") => {
                let hit = gone(&p.text());
                cut_any |= hit;
                !hit
            }
            XNode::Element(b) if b.name == "Branch" => {
                let below = cut(b, gone);
                cut_any |= below;
                !(below && leads_nowhere(b))
            }
            _ => true,
        });
        cut_any
    }
    sys.children.retain_mut(|c| match c {
        XNode::Element(l) if l.name == "Line" => match remap.kind {
            PortKind::Out => !l.prop("Src").is_some_and(|v| gone(&v)),
            _ => !(cut(l, &gone) && leads_nowhere(l)),
        },
        _ => true,
    });
    fn renumber(
        e: &mut XElem,
        key: &str,
        index_of: &dyn Fn(&str) -> Option<u32>,
        new_index: &dyn Fn(u32) -> Option<u32>,
        sid: &str,
        kind: PortKind,
        deep: bool,
    ) {
        if let Some(n) = e.prop(key).and_then(|v| index_of(&v)).and_then(new_index) {
            if let Some(p) = e.prop_mut(key) {
                p.set_text(&format!("{sid}#{}:{n}", kind.token()));
            }
        }
        if deep {
            for b in e.elements_mut().filter(|b| b.name == "Branch") {
                renumber(b, key, index_of, new_index, sid, kind, true);
            }
        }
    }
    for line in sys.elements_mut().filter(|l| l.name == "Line") {
        renumber(
            line,
            key,
            &index_of,
            &new_index,
            sid,
            remap.kind,
            remap.kind != PortKind::Out,
        );
    }
    Ok(())
}

/// Whether the endpoint value `v` (`12#out:1`) is `ep`.
fn is_endpoint(v: &str, ep: &Endpoint, default_kind: PortKind) -> bool {
    parse_endpoint(v, default_kind).is_some_and(|(sid, port)| sid == ep.block.0 && port == ep.port)
}

/// Whether the line or branch `e`, or a branch below it, has a destination
/// `hit` matches. Physical-connection branches store their port as `Src`.
fn reaches(e: &XElem, hit: &dyn Fn(&str) -> bool, is_line: bool) -> bool {
    let dst = |key| e.prop(key).is_some_and(|v| hit(&v));
    dst("Dst")
        || (!is_line && dst("Src"))
        || e.elements()
            .any(|b| b.name == "Branch" && reaches(b, hit, false))
}

/// The whitespace preceding `e`'s first child element: the newline and
/// indentation new children should get. Empty for unindented documents.
fn indent_in(e: &XElem) -> String {
    let first = e
        .children
        .iter()
        .position(|c| matches!(c, XNode::Element(_)));
    match first.and_then(|i| i.checked_sub(1)).map(|i| &e.children[i]) {
        Some(XNode::Text(t)) if t.trim().is_empty() && t.contains('\n') => t.clone(),
        _ => String::new(),
    }
}

/// A new element that will sit after `indent`, closing on its own line.
fn new_element(name: &str, indent: &str) -> XElem {
    let mut e = XElem::new(name);
    if !indent.is_empty() {
        e.children.push(XNode::Text(indent.to_string()));
    }
    e
}

/// One level deeper than `indent`.
fn deeper(indent: &str) -> String {
    if indent.is_empty() {
        String::new()
    } else {
        format!("{indent}  ")
    }
}

/// Insert `child` at `at` in `parent`, on its own line after `indent`.
fn insert_child(parent: &mut XElem, at: usize, child: XElem, indent: &str) {
    parent.children.insert(at, XNode::Element(child));
    if !indent.is_empty() {
        parent.children.insert(at, XNode::Text(indent.to_string()));
    }
    parent.empty = false;
}

/// Index just before the whitespace that precedes `e`'s end tag.
fn end_of(e: &XElem) -> usize {
    match e.children.last() {
        Some(XNode::Text(t)) if t.trim().is_empty() => e.children.len() - 1,
        _ => e.children.len(),
    }
}

/// Index after the last child element named one of `after`, or the end.
fn after_last(e: &XElem, after: &[&str]) -> usize {
    e.children
        .iter()
        .rposition(|c| matches!(c, XNode::Element(x) if after.contains(&x.name.as_str())))
        .map_or_else(|| end_of(e), |i| i + 1)
}

/// Append `<P Name="name">value</P>` to a new element built by
/// [`new_element`] after `indent`.
fn push_new_prop(e: &mut XElem, name: &str, value: &str, indent: &str) {
    let mut p = XElem::new("P");
    p.set_attr("Name", name);
    p.set_text(value);
    insert_child(e, end_of(e), p, &deeper(indent));
}

fn add_block(sys: &mut XElem, block: &Block) {
    let indent = indent_in(sys);
    let mut b = new_element("Block", &indent);
    b.set_attr("BlockType", &block.block_type);
    b.set_attr("Name", &block.name);
    b.set_attr("SID", &block.id.0);
    push_new_prop(&mut b, "Ports", &format_ports(&block.ports), &indent);
    push_new_prop(&mut b, "Position", &format_rect(&block.position), &indent);
    for (key, value) in &block.parameters {
        push_new_prop(&mut b, key, value, &indent);
    }
    let at = after_last(sys, &["Block"]);
    insert_child(sys, at, b, &indent);
}

/// Write port counts, in the `<PortCounts>` form if the block uses it.
fn set_ports(block: &mut XElem, ports: &PortCounts) {
    if let Some(pc) = block.elements_mut().find(|e| e.name == "PortCounts") {
        let counts = [
            ("in", ports.inputs),
            ("out", ports.outputs),
            ("enable", ports.enable),
            ("trigger", ports.trigger),
            ("state", ports.state),
            ("lconn", ports.lconn),
            ("rconn", ports.rconn),
            ("ifaction", ports.ifaction),
            ("reset", ports.reset),
        ];
        pc.attrs
            .retain(|(k, _)| !counts.iter().any(|(name, _)| k == name));
        for (name, n) in counts.into_iter().filter(|&(_, n)| n > 0) {
            pc.set_attr(name, &n.to_string());
        }
        return;
    }
    match block.prop_mut("Ports") {
        Some(p) => p.set_text(&format_ports(ports)),
        None => block.push_prop("Ports", &format_ports(ports)),
    }
}

/// Mirror of the IR's connect: branch the source's existing line (its
/// destination becoming the first branch), or start a new line.
fn connect(sys: &mut XElem, src: &Endpoint, dst: &Endpoint) {
    let sys_indent = indent_in(sys);
    let line = sys.elements_mut().find(|l| {
        l.name == "Line"
            && l.prop("Src")
                .is_some_and(|v| is_endpoint(&v, src, PortKind::Out))
    });
    let Some(line) = line else {
        let mut line = new_element("Line", &sys_indent);
        push_new_prop(&mut line, "Src", &src.to_string(), &sys_indent);
        push_new_prop(&mut line, "Dst", &dst.to_string(), &sys_indent);
        let at = after_last(sys, &["Line", "Block"]);
        insert_child(sys, at, line, &sys_indent);
        return;
    };
    let indent = indent_in(line);
    let old = line.children.iter().position(
        |c| matches!(c, XNode::Element(p) if p.name == "P" && p.attr("Name").as_deref() == Some("Dst")),
    );
    if let Some(i) = old {
        let XNode::Element(p) = line.children.remove(i) else {
            unreachable!("position matched an element")
        };
        if i > 0 && matches!(&line.children[i - 1], XNode::Text(t) if t.trim().is_empty()) {
            line.children.remove(i - 1);
        }
        let mut branch = new_element("Branch", &indent);
        let end = end_of(&branch);
        insert_child(&mut branch, end, p, &deeper(&indent));
        // Before the first branch and the indentation preceding it.
        let at = match line
            .children
            .iter()
            .position(|c| matches!(c, XNode::Element(b) if b.name == "Branch"))
        {
            Some(i)
                if i > 0
                    && matches!(&line.children[i - 1], XNode::Text(t) if t.trim().is_empty()) =>
            {
                i - 1
            }
            Some(i) => i,
            None => end_of(line),
        };
        insert_child(line, at, branch, &indent);
    }
    let mut branch = new_element("Branch", &indent);
    push_new_prop(&mut branch, "Dst", &dst.to_string(), &indent);
    let at = end_of(line);
    insert_child(line, at, branch, &indent);
}

pub(super) fn apply(bytes: &[u8], edits: &[Resolved]) -> Result<Vec<u8>, ImportError> {
    let zip_err = |e: zip::result::ZipError| ImportError::Zip(e.to_string());
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;

    // Parse every part that can hold blocks.
    let mut parts: Vec<(String, Document, bool)> = Vec::new();
    let mut read = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(zip_err)?;
        let name = file.name().to_string();
        if !is_block_part(&name) {
            continue;
        }
        let mut buf = String::new();
        (&mut file)
            .take(MAX_UNCOMPRESSED_BYTES - read + 1)
            .read_to_string(&mut buf)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        read += buf.len() as u64;
        if read > MAX_UNCOMPRESSED_BYTES {
            return Err(ImportError::TooLarge);
        }
        parts.push((name, dom::parse(&buf)?, false));
    }

    for resolved in edits {
        let at = locate(&parts, &resolved.system)?;
        system_at(&mut parts, &at)
            .map(|sys| apply_edit(sys, resolved))
            .ok_or_else(|| ImportError::Xml("empty document".into()))??;
        if let Some(boundary) = &resolved.boundary {
            let at = locate(&parts, &boundary.system)?;
            system_at(&mut parts, &at)
                .map(|sys| apply_boundary(sys, boundary))
                .ok_or_else(|| ImportError::Xml("empty document".into()))??;
        }
        let allocated = match &resolved.edit {
            Edit::AddAnnotation { id, .. } => Some(id.as_str()),
            _ => resolved.added.as_ref().map(|block| block.id.0.as_str()),
        };
        if let Some(sid) = allocated {
            let at = locate(&parts, &[])?;
            let root = system_at(&mut parts, &at)
                .ok_or_else(|| ImportError::Xml("empty document".into()))?;
            match root.prop_mut(SID_WATERMARK) {
                Some(watermark) => watermark.set_text(sid),
                // With the system's other properties, ahead of its blocks.
                None => {
                    let mut p = XElem::new("P");
                    p.set_attr("Name", SID_WATERMARK);
                    p.set_text(sid);
                    let (at, indent) = (after_last(root, &["P"]), indent_in(root));
                    insert_child(root, at, p, &indent);
                }
            }
        }
    }

    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let name = archive.by_index_raw(i).map_err(zip_err)?.name().to_string();
        match parts.iter().find(|(n, _, changed)| *changed && *n == name) {
            Some((_, doc, _)) => {
                let options =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
                out.start_file(name, options).map_err(zip_err)?;
                out.write_all(doc.to_xml().as_bytes())
                    .map_err(|e| ImportError::Zip(e.to_string()))?;
            }
            None => out
                .raw_copy_file(archive.by_index_raw(i).map_err(zip_err)?)
                .map_err(zip_err)?,
        }
    }
    Ok(out.finish().map_err(zip_err)?.into_inner())
}

fn set_route_points(element: &mut XElem, points: &super::RoutePoints) -> Result<(), ImportError> {
    if element.elements().filter(|b| b.name == "Branch").count() != points.branches.len() {
        return Err(ImportError::Edit(
            "serialized route topology differs from imported model".into(),
        ));
    }
    if points.value == "[]" {
        element.children.retain(|c|!matches!(c,XNode::Element(p) if p.name=="P" && p.attr("Name").as_deref()==Some("Points")));
    } else if let Some(p) = element.prop_mut("Points") {
        p.set_text(&points.value);
    } else {
        element.push_prop("Points", &points.value);
    }
    for (branch, p) in element
        .elements_mut()
        .filter(|b| b.name == "Branch")
        .zip(&points.branches)
    {
        set_route_points(branch, p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slx(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in parts {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(text.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn part(bytes: &[u8], name: &str) -> String {
        let mut a = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        a.by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    /// Rename block `id` in the system at name path `system`. The SLX
    /// patcher finds the block by SID, so its current name is not needed.
    fn rename(system: &[&str], id: &str, name: &str) -> Resolved {
        Resolved {
            edit: Edit::RenameBlock {
                system: vec![],
                id: id.into(),
                name: name.into(),
            },
            system: system.iter().map(|s| s.to_string()).collect(),
            names: Default::default(),
            added: None,
            route: None,
            ports: None,
            renumbered: Vec::new(),
            boundary: None,
        }
    }

    const NESTED: &str = "<ModelInformation><Model><System>\
        <Block BlockType=\"Gain\" Name=\"a\" SID=\"1\"><Unknown x='q\"r'/></Block>\
        <Block BlockType=\"SubSystem\" Name=\"S\" SID=\"2\"><System>\
        <Block BlockType=\"Gain\" Name=\"b\" SID=\"1\"/>\
        </System></Block>\
        </System></Model></ModelInformation>";

    #[test]
    fn edits_resolve_the_block_in_the_named_system() {
        let bytes = slx(&[(ROOT_PART, NESTED), ("other/part.bin", "untouched")]);
        let out = apply(&bytes, &[rename(&["S"], "1", "inner")]).unwrap();
        let xml = part(&out, ROOT_PART);
        assert!(xml.contains("Name=\"a\" SID=\"1\""), "{xml}");
        assert!(xml.contains("Name=\"inner\" SID=\"1\""), "{xml}");
        assert!(xml.contains("<Unknown x=\"q&quot;r\"/>"), "{xml}");
        assert_eq!(part(&out, "other/part.bin"), "untouched");

        let out = apply(&bytes, &[rename(&[], "1", "outer")]).unwrap();
        assert!(part(&out, ROOT_PART).contains("Name=\"b\" SID=\"1\""));
        assert!(apply(&bytes, &[rename(&["Missing"], "1", "x")]).is_err());
    }

    #[test]
    fn split_system_parts_are_followed() {
        let root = "<ModelInformation><Model><System>\
            <Block BlockType=\"SubSystem\" Name=\"S\" SID=\"2\"><System Ref=\"system_2\"/></Block>\
            </System></Model></ModelInformation>";
        let sub = "<System><Block BlockType=\"Gain\" Name=\"b\" SID=\"3\"/></System>";
        let bytes = slx(&[(ROOT_PART, root), ("simulink/systems/system_2.xml", sub)]);
        let out = apply(&bytes, &[rename(&["S"], "3", "c")]).unwrap();
        assert_eq!(part(&out, ROOT_PART), root);
        assert!(part(&out, "simulink/systems/system_2.xml").contains("Name=\"c\""));

        let wrong = slx(&[
            (ROOT_PART, root),
            ("simulink/systems/system_2.xml", "<Other/>"),
        ]);
        assert!(apply(&wrong, &[rename(&["S"], "3", "c")]).is_err());
        let missing = slx(&[(ROOT_PART, root)]);
        assert!(apply(&missing, &[rename(&["S"], "3", "c")]).is_err());
    }

    #[test]
    fn connecting_a_driven_source_branches_its_line_in_place() {
        let root = "<ModelInformation>\n  <Model>\n    <System>\n      <P Name=\"SIDHighWatermark\">3</P>\n      <Block BlockType=\"Gain\" Name=\"a\" SID=\"1\"/>\n      <Line>\n        <P Name=\"Src\">1#out:1</P>\n        <P Name=\"Points\">[20, 0]</P>\n        <P Name=\"Dst\">2#in:1</P>\n      </Line>\n    </System>\n  </Model>\n</ModelInformation>";
        let bytes = slx(&[(ROOT_PART, root)]);
        let ep = |s: &str| {
            let (block, port) = s.split_once('#').unwrap();
            Endpoint {
                block: block.into(),
                port: crate::convert::parse_port(port, PortKind::In).unwrap(),
            }
        };
        let resolved = |edit| Resolved {
            edit,
            system: vec![],
            names: Default::default(),
            added: None,
            route: None,
            ports: None,
            renumbered: Vec::new(),
            boundary: None,
        };
        let gain = Block {
            id: "4".into(),
            block_type: "Gain".into(),
            name: "b".into(),
            position: unlinked_model::Rect::new(0.0, 0.0, 30.0, 30.0),
            orientation: Default::default(),
            mirrored: false,
            ports: PortCounts::from_slice(&[1, 1]),
            parameters: [("Gain".to_string(), "1".to_string())].into(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: Default::default(),
            interface: None,
        };
        let mut add = resolved(Edit::AddBlock {
            system: vec![],
            id: "4".into(),
            block_type: "Gain".into(),
            name: "b".into(),
            position: gain.position,
        });
        add.added = Some(gain);
        let connect = resolved(Edit::Connect {
            system: vec![],
            src: ep("1#out:1"),
            dst: ep("4#in:1"),
        });
        let out = apply(&bytes, &[add, connect]).unwrap();
        let xml = part(&out, ROOT_PART);
        assert_eq!(
            xml,
            "<ModelInformation>\n  <Model>\n    <System>\n      <P Name=\"SIDHighWatermark\">4</P>\n      <Block BlockType=\"Gain\" Name=\"a\" SID=\"1\"/>\n      <Block BlockType=\"Gain\" Name=\"b\" SID=\"4\">\n        <P Name=\"Ports\">[1, 1]</P>\n        <P Name=\"Position\">[0, 0, 30, 30]</P>\n        <P Name=\"Gain\">1</P>\n      </Block>\n      <Line>\n        <P Name=\"Src\">1#out:1</P>\n        <P Name=\"Points\">[20, 0]</P>\n        <Branch>\n          <P Name=\"Dst\">2#in:1</P>\n        </Branch>\n        <Branch>\n          <P Name=\"Dst\">4#in:1</P>\n        </Branch>\n      </Line>\n    </System>\n  </Model>\n</ModelInformation>"
        );
    }

    #[test]
    fn duplicate_sids_in_one_system_are_rejected() {
        let dup = "<ModelInformation><Model><System>\
            <Block Name=\"a\" SID=\"1\"/><Block Name=\"b\" SID=\"1\"/>\
            </System></Model></ModelInformation>";
        let bytes = slx(&[(ROOT_PART, dup)]);
        assert!(matches!(
            apply(&bytes, &[rename(&[], "1", "c")]),
            Err(ImportError::Edit(_))
        ));
    }

    /// Every block part of every corpus SLX file serializes back to exactly
    /// its original bytes.
    #[test]
    fn corpus_parts_roundtrip_byte_identical() {
        let dir = match std::env::var_os("UNLINKED_TEST_CASES") {
            Some(d) => std::path::PathBuf::from(d),
            None => return,
        };
        let mut stack = vec![dir];
        let mut checked = 0;
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("slx") {
                    continue;
                }
                let bytes = std::fs::read(&p).unwrap();
                let mut a = ZipArchive::new(Cursor::new(bytes)).unwrap();
                for i in 0..a.len() {
                    let mut f = a.by_index(i).unwrap();
                    if !is_block_part(f.name()) {
                        continue;
                    }
                    let name = f.name().to_string();
                    let mut text = String::new();
                    f.read_to_string(&mut text).unwrap();
                    let doc = dom::parse(&text).unwrap();
                    assert!(doc.to_xml() == text, "{}: {name} changed", p.display());
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }
}
