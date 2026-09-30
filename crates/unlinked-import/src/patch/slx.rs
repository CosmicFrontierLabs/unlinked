//! Apply edits to an SLX package. The system an edit targets is resolved by
//! its path of block names from the root, following `<System Ref=...>`
//! placeholders into split `systems/*.xml` parts, and the block by SID
//! within that system only. Only parts that change are rewritten; every
//! other zip entry is copied raw.

use super::dom::{self, Document, XElem, XNode};
use crate::{ImportError, MAX_DEPTH, MAX_UNCOMPRESSED_BYTES};
use std::io::{Cursor, Read, Write};
use unlinked_model::edit::Edit;
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

fn part_root<'a>(
    parts: &'a [(String, Document, bool)],
    part: usize,
) -> Result<&'a XElem, ImportError> {
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

/// Apply `edit` to its block, a direct child of the system element `parent`.
fn apply_edit(parent: &mut XElem, edit: &Edit) -> Result<(), ImportError> {
    let sid = edit.block().0.clone();
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
    Ok(())
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
        let at = locate(&parts, edit.system())?;
        let (_, doc, changed) = &mut parts[at.part];
        let root = doc
            .root_mut()
            .ok_or_else(|| ImportError::Xml("empty document".into()))?;
        apply_edit(descend_mut(root, &at.path), edit)?;
        *changed = true;
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

    fn rename(system: &[&str], id: &str, name: &str) -> Edit {
        Edit::RenameBlock {
            system: system.iter().map(|s| s.to_string()).collect(),
            id: id.into(),
            name: name.into(),
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
