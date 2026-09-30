//! Apply edits to an SLX package. Blocks are located by SID across
//! `blockdiagram.xml` and the split `systems/*.xml` parts; only parts that
//! change are rewritten, every other zip entry is copied raw.

use super::dom::{self, Document, XElem, XNode};
use crate::{ImportError, MAX_UNCOMPRESSED_BYTES};
use std::io::{Cursor, Read, Write};
use unlinked_model::edit::Edit;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn is_block_part(name: &str) -> bool {
    name == "simulink/blockdiagram.xml"
        || (name.starts_with("simulink/systems/") && name.ends_with(".xml"))
}

fn is_block(n: &XNode, sid: &str) -> bool {
    matches!(n, XNode::Element(b) if b.name == "Block" && b.attr("SID").as_deref() == Some(sid))
}

fn contains_block(e: &XElem, sid: &str) -> bool {
    e.children.iter().any(|c| match c {
        XNode::Element(child) => is_block(c, sid) || contains_block(child, sid),
        _ => false,
    })
}

/// The element whose direct child is block `sid`, and that child's index.
fn find_parent<'a>(e: &'a mut XElem, sid: &str) -> Option<(&'a mut XElem, usize)> {
    if let Some(i) = e.children.iter().position(|c| is_block(c, sid)) {
        return Some((e, i));
    }
    let i = e
        .children
        .iter()
        .position(|c| matches!(c, XNode::Element(child) if contains_block(child, sid)))?;
    match &mut e.children[i] {
        XNode::Element(child) => find_parent(child, sid),
        _ => None,
    }
}

fn element_mut(parent: &mut XElem, i: usize) -> &mut XElem {
    match &mut parent.children[i] {
        XNode::Element(e) => e,
        _ => unreachable!("find_parent returns element indices"),
    }
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

/// Remove endpoints at `sid` from a line or branch; returns whether it
/// still leads anywhere. Physical-connection branches store their port as
/// `Src`, so both keys count as destinations below the line's own source.
fn prune(e: &mut XElem, sid: &str, is_line: bool) -> bool {
    e.children.retain_mut(|c| match c {
        XNode::Element(p) if p.name == "P" => {
            let name = p.attr("Name");
            let dst =
                name.as_deref() == Some("Dst") || (!is_line && name.as_deref() == Some("Src"));
            !(dst && refers_to_value(&p.text(), sid))
        }
        XNode::Element(b) if b.name == "Branch" => prune(b, sid, false),
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

fn apply_edit(doc: &mut Document, edit: &Edit) -> Result<bool, ImportError> {
    let sid = edit.block().0.clone();
    let Some(root) = doc.root_mut() else {
        return Ok(false);
    };
    let Some((parent, i)) = find_parent(root, &sid) else {
        return Ok(false);
    };
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
        Edit::SetParameter { name, value, .. } => {
            set_parameter(element_mut(parent, i), name, value)
        }
        Edit::RenameBlock { name, .. } => element_mut(parent, i).set_attr("Name", name),
        Edit::DeleteBlock { .. } => {
            parent.children.remove(i);
            parent.children.retain_mut(|c| match c {
                // Leave unrelated (possibly already dangling) lines alone.
                XNode::Element(l) if l.name == "Line" && touches(l, &sid) => {
                    !refers_to(l, "Src", &sid) && prune(l, &sid, true)
                }
                _ => true,
            });
        }
    }
    Ok(true)
}

pub fn apply(bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ImportError> {
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

    for edit in edits {
        let mut found = false;
        for (_, doc, changed) in parts.iter_mut() {
            if apply_edit(doc, edit)? {
                *changed = true;
                found = true;
                break;
            }
        }
        if !found {
            return Err(ImportError::Xml(format!(
                "block {} not found",
                edit.block()
            )));
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
