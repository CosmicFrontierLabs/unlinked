//! Apply edits to MDL text. The file is held as a tree of sections whose
//! lines are kept verbatim, so untouched content is reproduced exactly.
//! Blocks are found by the containing system's path and the block name
//! (MDL lines refer to blocks by name).

#[path = "mdl_expand.rs"]
mod expand;
#[path = "mdl_hierarchy.rs"]
mod hierarchy;
use unlinked_model::expand::removable_property as expansion_property;

use super::{format_ports, parse_endpoint};
use super::{Boundary, Resolved};
use crate::convert::parse_port;
use crate::{ImportError, MAX_DEPTH, MAX_NODES};
use unlinked_model::edit::{config_writes, Edit, SID_WATERMARK};
use unlinked_model::geometry::to_rotation;
use unlinked_model::{Block, PortKind, PortRef};
use unlinked_model::{Orientation, Rect};

#[derive(Debug, Clone)]
enum Item {
    /// A property: its `Key value` line plus any `"..."` continuation lines.
    Prop {
        key: String,
        lines: Vec<String>,
    },
    Section(Section),
    /// Blank lines, comments and anything unrecognized.
    Raw(String),
}

#[derive(Debug, Clone)]
struct Section {
    tag: String,
    header: String,
    items: Vec<Item>,
    footer: String,
}

/// Decode an MDL value (quoted strings with escapes and continuations, or
/// bare text).
fn decode(lines: &[String]) -> String {
    let first = lines[0].trim();
    let rest = first
        .find(char::is_whitespace)
        .map(|i| first[i..].trim())
        .unwrap_or("");
    if !rest.starts_with('"') {
        return rest.to_string();
    }
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let piece = if i == 0 { rest } else { l.trim() };
        let body = piece
            .strip_prefix('"')
            .and_then(|b| b.strip_suffix('"'))
            .unwrap_or(piece);
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(o @ ('"' | '\\' | '\'')) => out.push(o),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        }
    }
    out
}

/// Whether `value` can be written unquoted without changing the file's
/// structure: a single non-empty token with no quotes, braces or newlines.
fn is_bare_token(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '{' | '}' | '#'))
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

impl Section {
    fn prop(&self, key: &str) -> Option<String> {
        self.items.iter().find_map(|i| match i {
            Item::Prop { key: k, lines } if k == key => Some(decode(lines)),
            _ => None,
        })
    }

    fn was_quoted(&self, key: &str) -> Option<bool> {
        self.items.iter().find_map(|i| match i {
            Item::Prop { key: k, lines } if k == key => {
                Some(lines[0].trim()[key.len()..].trim_start().starts_with('"'))
            }
            _ => None,
        })
    }

    /// Set `key` to `value`, written quoted unless `bare`; replaces the
    /// existing property in place or appends one before the closing brace.
    fn set_prop(&mut self, key: &str, value: &str, bare: bool) {
        let text = if bare {
            value.to_string()
        } else {
            quote(value)
        };
        let indent = self
            .items
            .iter()
            .find_map(|i| match i {
                Item::Prop { lines, .. } => Some(indent_of(&lines[0]).to_string()),
                _ => None,
            })
            .unwrap_or_else(|| format!("{}  ", indent_of(&self.header)));
        let line = format!("{indent}{key}\t{text}{}", self.eol());
        match self
            .items
            .iter_mut()
            .find(|i| matches!(i, Item::Prop { key: k, .. } if k == key))
        {
            Some(Item::Prop { lines, .. }) => *lines = vec![line],
            _ => {
                // Properties precede nested sections in MDL.
                let at = self
                    .items
                    .iter()
                    .position(|i| matches!(i, Item::Section(_)))
                    .unwrap_or(self.items.len());
                self.items.insert(
                    at,
                    Item::Prop {
                        key: key.to_string(),
                        lines: vec![line],
                    },
                );
            }
        }
    }

    fn remove_prop(&mut self, key: &str) {
        self.items
            .retain(|i| !matches!(i, Item::Prop { key: k, .. } if k == key));
    }

    fn sections(&self) -> impl Iterator<Item = &Section> {
        self.items.iter().filter_map(|i| match i {
            Item::Section(s) => Some(s),
            _ => None,
        })
    }

    fn sections_mut(&mut self) -> impl Iterator<Item = &mut Section> {
        self.items.iter_mut().filter_map(|i| match i {
            Item::Section(s) => Some(s),
            _ => None,
        })
    }

    fn write(&self, out: &mut String) {
        out.push_str(&self.header);
        for i in &self.items {
            write_item(i, out);
        }
        out.push_str(&self.footer);
    }

    /// The line terminator this section uses.
    fn eol(&self) -> &str {
        eol_of(&self.header)
    }
}

fn eol_of(line: &str) -> &str {
    &line[line.trim_end_matches(['\r', '\n']).len()..]
}

fn write_item(item: &Item, out: &mut String) {
    match item {
        Item::Prop { lines, .. } => lines.iter().for_each(|l| out.push_str(l)),
        Item::Section(s) => s.write(out),
        Item::Raw(l) => out.push_str(l),
    }
}

struct MdlFile {
    items: Vec<Item>,
    /// Everything from an embedded OPC package marker on, verbatim.
    tail: String,
}

fn parse(text: &str) -> Result<MdlFile, ImportError> {
    let err = |msg: &str| ImportError::Mdl(msg.to_string());
    let mut stack: Vec<Section> = Vec::new();
    let mut top: Vec<Item> = Vec::new();
    let mut tail = String::new();
    // Lines keep their own terminators (LF, CRLF or none at EOF).
    let mut lines = text.split_inclusive('\n').peekable();
    let mut budget = MAX_NODES;
    let mut offset = 0usize;
    while let Some(line) = lines.next() {
        offset += line.len();
        budget = budget.checked_sub(1).ok_or_else(|| err("too many lines"))?;
        let t = line.trim();
        if t.starts_with("__MWOPC_PACKAGE_BEGIN__") {
            tail = text[offset - line.len()..].to_string();
            break;
        }
        let item = if t == "}" {
            let mut s = stack.pop().ok_or_else(|| err("unbalanced '}'"))?;
            s.footer = line.to_string();
            Item::Section(s)
        } else if let Some(tag) = t.strip_suffix('{') {
            if stack.len() >= MAX_DEPTH {
                return Err(err("nesting too deep"));
            }
            stack.push(Section {
                tag: tag.trim().to_string(),
                header: line.to_string(),
                items: Vec::new(),
                footer: String::new(),
            });
            continue;
        } else if t.is_empty() || t.starts_with('#') || t.starts_with('"') {
            Item::Raw(line.to_string())
        } else {
            let key = t.split(char::is_whitespace).next().unwrap_or(t).to_string();
            let mut prop_lines = vec![line.to_string()];
            let quoted = t[key.len()..].trim_start().starts_with('"');
            while quoted && lines.peek().is_some_and(|n| n.trim().starts_with('"')) {
                let next = lines.next().unwrap();
                offset += next.len();
                prop_lines.push(next.to_string());
            }
            Item::Prop {
                key,
                lines: prop_lines,
            }
        };
        match stack.last_mut() {
            Some(s) => s.items.push(item),
            None => top.push(item),
        }
    }
    if !stack.is_empty() {
        return Err(err("unexpected end of file"));
    }
    Ok(MdlFile { items: top, tail })
}

impl MdlFile {
    fn to_text(&self) -> String {
        let mut out = String::new();
        for i in &self.items {
            write_item(i, &mut out);
        }
        out.push_str(&self.tail);
        out
    }

    fn system_mut(&mut self, path: &[String]) -> Option<&mut Section> {
        let model = self.items.iter_mut().find_map(|i| match i {
            Item::Section(s) if s.tag == "Model" || s.tag == "Library" => Some(s),
            _ => None,
        })?;
        let mut sys = model.sections_mut().find(|s| s.tag == "System")?;
        for name in path {
            let is_named =
                |b: &Section| b.tag == "Block" && b.prop("Name").as_deref() == Some(name.as_str());
            // An ambiguous name has no single target.
            if sys.sections().filter(|b| is_named(b)).count() != 1 {
                return None;
            }
            sys = sys
                .sections_mut()
                .find(|b| is_named(b))?
                .sections_mut()
                .find(|s| s.tag == "System")?;
        }
        Some(sys)
    }
}

/// Whether a line or branch section refers to block `name` (by name) or
/// `sid` (by `SID#port`) under any of `keys`.
fn endpoint_is(s: &Section, keys: &[&str], name: &str, sid: Option<&str>) -> bool {
    keys.iter().any(|k| {
        s.prop(k).is_some_and(|v| {
            if k.ends_with("Block") {
                v == name
            } else {
                sid.is_some_and(|sid| v.split_once('#').is_some_and(|(b, _)| b == sid))
            }
        })
    })
}

const SRC_KEYS: [&str; 2] = ["SrcBlock", "Src"];
const DST_KEYS: [&str; 2] = ["DstBlock", "Dst"];

fn touches(s: &Section, name: &str, sid: Option<&str>) -> bool {
    endpoint_is(s, &SRC_KEYS, name, sid)
        || endpoint_is(s, &DST_KEYS, name, sid)
        || s.sections()
            .any(|b| b.tag == "Branch" && touches(b, name, sid))
}

fn clear_points(s: &mut Section) {
    s.remove_prop("Points");
    for b in s.sections_mut().filter(|b| b.tag == "Branch") {
        clear_points(b);
    }
}

fn rename_refs(s: &mut Section, old: &str, new: &str) {
    for key in ["SrcBlock", "DstBlock"] {
        if s.prop(key).as_deref() == Some(old) {
            s.set_prop(key, new, false);
        }
    }
    for b in s.sections_mut().filter(|b| b.tag == "Branch") {
        rename_refs(b, old, new);
    }
}

/// Remove the endpoints `hit` matches (given whether `s` is the line
/// itself) and branches left leading nowhere; returns whether `s` still
/// leads anywhere. Physical-connection branches store their port as `Src`.
fn prune(s: &mut Section, hit: &dyn Fn(&Section, bool) -> bool, is_line: bool) -> bool {
    if hit(s, is_line) {
        for k in ["DstBlock", "DstPort", "Dst"] {
            s.remove_prop(k);
        }
        if !is_line {
            for k in ["SrcBlock", "SrcPort", "Src"] {
                s.remove_prop(k);
            }
        }
    }
    s.items.retain_mut(|i| match i {
        Item::Section(b) if b.tag == "Branch" => prune(b, hit, false),
        _ => true,
    });
    s.prop("DstBlock").is_some()
        || s.prop("Dst").is_some()
        || (!is_line && (s.prop("SrcBlock").is_some() || s.prop("Src").is_some()))
        || s.sections().any(|b| b.tag == "Branch")
}

/// Set a mask parameter: new-style `Simulink.MaskParameter` objects, or the
/// old `MaskVariables`/`MaskValueString` pair. Returns whether one matched.
fn set_mask_parameter(block: &mut Section, name: &str, value: &str) -> bool {
    fn visit(s: &mut Section, name: &str, value: &str, in_params: bool) -> bool {
        if in_params && s.tag == "Object" && s.prop("Name").as_deref() == Some(name) {
            s.set_prop("Value", value, false);
            return true;
        }
        let params =
            s.tag == "Array" && s.prop("Type").as_deref() == Some("Simulink.MaskParameter");
        s.sections_mut().any(|c| visit(c, name, value, params))
    }
    if block.sections_mut().any(|c| visit(c, name, value, false)) {
        return true;
    }
    let (Some(vars), Some(values)) = (block.prop("MaskVariables"), block.prop("MaskValueString"))
    else {
        return false;
    };
    let Some(slot) = vars
        .split(';')
        .filter_map(|v| v.split('=').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .position(|v| v == name)
    else {
        return false;
    };
    let mut parts: Vec<String> = values.split('|').map(str::to_string).collect();
    if parts.len() <= slot {
        parts.resize(slot + 1, String::new());
    }
    parts[slot] = value.to_string();
    block.set_prop("MaskValueString", &parts.join("|"), false);
    true
}

fn format_rect(p: &Rect) -> String {
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

/// Preserve legacy point anchoring instead of replacing it with a zero-size box.
fn annotation_position(position: &Rect, existing: Option<&str>) -> String {
    let was_point = existing.is_none_or(|value| {
        value
            .split(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | ',' | ';'))
            .filter(|part| !part.is_empty())
            .count()
            == 2
    });
    if was_point && position.left == position.right && position.top == position.bottom {
        format!("[{}, {}]", position.left, position.top)
    } else {
        format_rect(position)
    }
}

fn apply_edit(file: &mut MdlFile, resolved: &Resolved) -> Result<(), ImportError> {
    let (edit, path) = (&resolved.edit, &resolved.system);
    if let Edit::SetConfig { key, value } = edit {
        return set_config(file, key, value);
    }
    let sys = file
        .system_mut(path)
        .ok_or_else(|| ImportError::Mdl(format!("no system at {path:?}")))?;
    if let Edit::ExpandSubsystem { .. } = edit {
        let plan = resolved
            .expansion
            .as_ref()
            .ok_or_else(|| ImportError::Edit("missing expansion plan".into()))?;
        expand::expand(sys, resolved, plan)?;
        let root = file
            .system_mut(&[])
            .ok_or_else(|| ImportError::Mdl("no root system".into()))?;
        root.set_prop(SID_WATERMARK, &plan.watermark.to_string(), false);
        return Ok(());
    }
    if let Edit::CreateSubsystem { .. } = edit {
        let plan = resolved
            .hierarchy
            .as_ref()
            .ok_or_else(|| ImportError::Edit("missing hierarchy plan".into()))?;
        hierarchy::create(sys, resolved, plan)?;
        let root = file
            .system_mut(&[])
            .ok_or_else(|| ImportError::Mdl("no root system".into()))?;
        root.set_prop(SID_WATERMARK, &plan.watermark.to_string(), false);
        return Ok(());
    }
    if let Edit::AddAnnotation {
        id, text, position, ..
    } = edit
    {
        let mut a = new_section("Annotation", &child_indent(sys), sys.eol());
        a.set_prop("SID", id, false);
        a.set_prop("Name", text, false);
        a.set_prop("Position", &annotation_position(position, None), true);
        insert_after(sys, &["Annotation"], a);
        let root = file
            .system_mut(&[])
            .ok_or_else(|| ImportError::Mdl("no root system".into()))?;
        root.set_prop(SID_WATERMARK, id, false);
        return Ok(());
    }
    let annotation = match edit {
        Edit::MoveAnnotation { target, .. }
        | Edit::SetAnnotationText { target, .. }
        | Edit::DeleteAnnotation { target, .. } => Some(target.index),
        _ => None,
    };
    if let Some(index) = annotation {
        let at = sys
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                matches!(item, Item::Section(s) if s.tag == "Annotation").then_some(i)
            })
            .nth(index)
            .ok_or_else(|| ImportError::Edit("serialized annotation missing".into()))?;
        if matches!(edit, Edit::DeleteAnnotation { .. }) {
            sys.items.remove(at);
        } else if let Item::Section(a) = &mut sys.items[at] {
            match edit {
                Edit::MoveAnnotation { position, .. } => {
                    let value = annotation_position(position, a.prop("Position").as_deref());
                    a.set_prop("Position", &value, true)
                }
                Edit::SetAnnotationText { text, .. } => {
                    let key = if a.prop("Text").is_some() {
                        "Text"
                    } else {
                        "Name"
                    };
                    a.set_prop(key, text, false);
                }
                _ => unreachable!(),
            }
        }
        return Ok(());
    }
    let id = match edit {
        Edit::CreateSubsystem { .. }
        | Edit::ExpandSubsystem { .. }
        | Edit::AddAnnotation { .. }
        | Edit::MoveAnnotation { .. }
        | Edit::SetAnnotationText { .. }
        | Edit::DeleteAnnotation { .. }
        | Edit::SetConfig { .. } => unreachable!("applied above"),
        Edit::AddBlock { .. } => {
            let block = resolved
                .added
                .as_ref()
                .ok_or_else(|| ImportError::Edit("the added block is missing".into()))?;
            add_block(sys, block);
            let root = file
                .system_mut(&[])
                .ok_or_else(|| ImportError::Mdl("no root system".into()))?;
            root.set_prop(SID_WATERMARK, &block.id.0, false);
            return Ok(());
        }
        Edit::Connect { src, dst, .. } => {
            let src = Port::new(sys, resolved.name(&src.block)?, src.port, PortKind::Out);
            let dst = Port::new(sys, resolved.name(&dst.block)?, dst.port, PortKind::In);
            connect(sys, &src, &dst);
            return Ok(());
        }
        Edit::Disconnect { dst, .. } => {
            let dst = Port::new(sys, resolved.name(&dst.block)?, dst.port, PortKind::In);
            // Physical-connection branches store their port as `Src`.
            let hit = |s: &Section, is_line: bool| {
                dst.is_at(s, DST_FORMS) || (!is_line && dst.is_at(s, SRC_FORMS))
            };
            sys.items.retain_mut(|item| match item {
                Item::Section(l) if l.tag == "Line" && reaches(l, &hit, true) => {
                    prune(l, &hit, true)
                }
                _ => true,
            });
            return Ok(());
        }
        Edit::SetSignalName { src, name, .. } => {
            let source = Port::new(sys, resolved.name(&src.block)?, src.port, PortKind::Out);
            let matches: Vec<usize> = sys
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| match item {
                    Item::Section(l) if l.tag == "Line" && source.is_at(l, SRC_FORMS) => Some(i),
                    _ => None,
                })
                .collect();
            if matches.len() != 1 {
                return Err(ImportError::Edit(
                    "serialized signal source missing or ambiguous".into(),
                ));
            }
            if let Item::Section(line) = &mut sys.items[matches[0]] {
                if name.is_empty() {
                    line.remove_prop("Name");
                } else {
                    line.set_prop("Name", name, false);
                }
            }
            return Ok(());
        }
        Edit::SetRoute { .. } | Edit::SetTrunkRoute { .. } => {
            let update = resolved
                .route
                .as_ref()
                .ok_or_else(|| ImportError::Edit("route update missing".into()))?;
            let src = Port::new(
                sys,
                resolved.name(&update.src.block)?,
                update.src.port,
                PortKind::Out,
            );
            let matches: Vec<usize> = sys
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| match item {
                    Item::Section(l) if l.tag == "Line" && src.is_at(l, SRC_FORMS) => Some(i),
                    _ => None,
                })
                .collect();
            if matches.len() != 1 {
                return Err(ImportError::Edit(
                    "serialized route source missing or ambiguous".into(),
                ));
            }
            if let Item::Section(line) = &mut sys.items[matches[0]] {
                set_route_points(line, &update.points)?;
            }
            return Ok(());
        }
        Edit::MoveBlock { id, .. }
        | Edit::SetParameter { id, .. }
        | Edit::RenameBlock { id, .. }
        | Edit::DeleteBlock { id, .. }
        | Edit::SetOrientation { id, .. } => id,
    };
    let name = resolved.name(id)?;
    let mut matches = sys.items.iter().enumerate().filter(|(_, i)| {
        matches!(i, Item::Section(b) if b.tag == "Block" && b.prop("Name").as_deref() == Some(name))
    });
    let i = match (matches.next(), matches.next()) {
        (Some((i, _)), None) => i,
        (None, _) => return Err(ImportError::Edit(format!("no block {name:?}"))),
        (Some(_), Some(_)) => {
            return Err(ImportError::Edit(format!(
                "block name {name:?} is ambiguous"
            )))
        }
    };
    fn block_at(sys: &mut Section, i: usize) -> &mut Section {
        match &mut sys.items[i] {
            Item::Section(b) => b,
            _ => unreachable!("position matched a section"),
        }
    }
    let sid = block_at(sys, i).prop("SID");
    let sid = sid.as_deref();
    match edit {
        Edit::MoveBlock { position, .. } => {
            block_at(sys, i).set_prop("Position", &format_rect(position), true);
            for line in sys
                .sections_mut()
                .filter(|l| l.tag == "Line" && touches(l, name, sid))
            {
                clear_points(line);
            }
        }
        Edit::SetOrientation {
            orientation,
            mirrored,
            ..
        } => {
            let b = block_at(sys, i);
            // Older files say `Orientation "left"`; keep that form unless
            // the block already uses rotation or needs a mirror.
            if b.prop("BlockRotation").is_none() && !mirrored {
                let word = match orientation {
                    Orientation::Right => "right",
                    Orientation::Left => "left",
                    Orientation::Up => "up",
                    Orientation::Down => "down",
                };
                b.set_prop("Orientation", word, false);
                b.remove_prop("BlockMirror");
            } else {
                let (rotation, mirror) = to_rotation(*orientation, *mirrored);
                b.remove_prop("Orientation");
                b.set_prop("BlockRotation", &rotation.to_string(), true);
                b.set_prop("BlockMirror", if mirror { "on" } else { "off" }, true);
            }
            for line in sys
                .sections_mut()
                .filter(|l| l.tag == "Line" && touches(l, name, sid))
            {
                clear_points(line);
            }
        }
        Edit::SetParameter {
            name: key, value, ..
        } => {
            let b = block_at(sys, i);
            if !set_mask_parameter(b, key, value) {
                let bare = b.was_quoted(key) == Some(false) && is_bare_token(value);
                b.set_prop(key, value, bare);
            }
            if let Some(ports) = &resolved.ports {
                b.set_prop("Ports", &format_ports(ports), true);
            }
        }
        Edit::RenameBlock { name: new, .. } => {
            block_at(sys, i).set_prop("Name", new, false);
            for line in sys.sections_mut().filter(|l| l.tag == "Line") {
                rename_refs(line, name, new);
            }
        }
        Edit::DeleteBlock { .. } => {
            sys.items.remove(i);
            let hit = |s: &Section, is_line: bool| {
                let keys: &[&str] = if is_line {
                    &DST_KEYS
                } else {
                    &["DstBlock", "Dst", "SrcBlock", "Src"]
                };
                endpoint_is(s, keys, name, sid)
            };
            sys.items.retain_mut(|item| match item {
                Item::Section(l) if l.tag == "Line" && touches(l, name, sid) => {
                    !endpoint_is(l, &SRC_KEYS, name, sid) && prune(l, &hit, true)
                }
                _ => true,
            });
        }
        Edit::CreateSubsystem { .. }
        | Edit::ExpandSubsystem { .. }
        | Edit::AddAnnotation { .. }
        | Edit::MoveAnnotation { .. }
        | Edit::SetAnnotationText { .. }
        | Edit::DeleteAnnotation { .. }
        | Edit::AddBlock { .. }
        | Edit::Connect { .. }
        | Edit::Disconnect { .. }
        | Edit::SetRoute { .. }
        | Edit::SetTrunkRoute { .. }
        | Edit::SetSignalName { .. }
        | Edit::SetConfig { .. } => {
            unreachable!("applied above")
        }
    }
    // Deleting or renumbering a port block renumbers its siblings.
    for (id, number) in &resolved.renumbered {
        let other = resolved.name(id)?;
        let Some(b) = sys
            .sections_mut()
            .find(|b| b.tag == "Block" && b.prop("Name").as_deref() == Some(other))
        else {
            continue;
        };
        let interface = b
            .sections_mut()
            .find(|l| l.tag == "List" && l.prop("ListType").as_deref() == Some("InterfaceData"));
        let bus = interface.is_some();
        if let Some(list) = interface {
            list.set_prop("PortNumber", number, false);
        }
        if !bus || b.prop("Port").is_some() {
            let bare = b.was_quoted("Port") == Some(false);
            b.set_prop("Port", number, bare);
        }
    }
    Ok(())
}

/// Write a solver setting where the importer reads it: the first solver
/// component of the model's configuration sets, or the `Model` section
/// itself in files without one.
fn set_config(file: &mut MdlFile, key: &str, value: &str) -> Result<(), ImportError> {
    fn solver(s: &mut Section) -> Option<&mut Section> {
        if s.tag == "Simulink.SolverCC" {
            return Some(s);
        }
        s.sections_mut().find_map(solver)
    }
    let model = file
        .items
        .iter_mut()
        .find_map(|i| match i {
            Item::Section(s) if s.tag == "Model" || s.tag == "Library" => Some(s),
            _ => None,
        })
        .ok_or_else(|| ImportError::Mdl("no Model section".into()))?;
    let has_solver = solver(model).is_some();
    let target = if has_solver {
        solver(model).expect("checked above")
    } else {
        model
    };
    for k in config_writes(key, |k| target.prop(k).is_some()) {
        let bare = target.was_quoted(k) == Some(false);
        target.set_prop(k, value, bare && is_bare_token(value));
    }
    Ok(())
}

/// Update the subsystem block around an edited system: its port count and
/// the connections outside it, per `boundary.remap`.
fn apply_boundary(file: &mut MdlFile, boundary: &Boundary) -> Result<(), ImportError> {
    let path = &boundary.system;
    let sys = file
        .system_mut(path)
        .ok_or_else(|| ImportError::Mdl(format!("no system at {path:?}")))?;
    let name = boundary.name.as_str();
    let block = sys
        .sections_mut()
        .find(|b| b.tag == "Block" && b.prop("Name").as_deref() == Some(name))
        .ok_or_else(|| ImportError::Edit(format!("no block {name:?}")))?;
    let sid = block.prop("SID");
    block.set_prop("Ports", &format_ports(&boundary.ports), true);
    let remap = &boundary.remap;
    let (forms, default_kind) = match remap.kind {
        PortKind::Out => (SRC_FORMS, PortKind::Out),
        _ => (DST_FORMS, PortKind::In),
    };
    // The port index an endpoint has on the subsystem block, if it is one.
    let index_of = |s: &Section| -> Option<u32> {
        let [sid_key, block_key, port_key] = forms;
        if let Some(v) = s.prop(sid_key) {
            let (block, port) = parse_endpoint(&v, default_kind)?;
            return (Some(block) == sid.as_deref() && port.kind == remap.kind)
                .then_some(port.index);
        }
        if s.prop(block_key).as_deref() != Some(name) {
            return None;
        }
        let port = parse_port(s.prop(port_key).as_deref().unwrap_or("1"), default_kind)?;
        (port.kind == remap.kind).then_some(port.index)
    };
    let new_index = |old: u32| remap.map.get(old as usize - 1).copied().flatten();
    let gone = |s: &Section| index_of(s).is_some_and(|i| i >= 1 && new_index(i).is_none());
    // Connections on removed ports go (the IR already refused them unless
    // disconnecting) and the rest move to their new numbers. As in the IR,
    // only what this cuts is removed; other dangling wiring stays.
    let leads_nowhere = |s: &Section| {
        DST_FORMS[..2].iter().all(|k| s.prop(k).is_none())
            && !s.sections().any(|b| b.tag == "Branch")
    };
    fn cut(
        s: &mut Section,
        gone: &dyn Fn(&Section) -> bool,
        leads_nowhere: &dyn Fn(&Section) -> bool,
    ) -> bool {
        let mut cut_any = gone(s);
        if cut_any {
            for key in DST_FORMS {
                s.remove_prop(key);
            }
        }
        s.items.retain_mut(|item| match item {
            Item::Section(b) if b.tag == "Branch" => {
                let below = cut(b, gone, leads_nowhere);
                cut_any |= below;
                !(below && leads_nowhere(b))
            }
            _ => true,
        });
        cut_any
    }
    sys.items.retain_mut(|item| match item {
        Item::Section(l) if l.tag == "Line" => match remap.kind {
            PortKind::Out => !gone(l),
            _ => !(cut(l, &gone, &leads_nowhere) && leads_nowhere(l)),
        },
        _ => true,
    });
    fn renumber(
        s: &mut Section,
        index_of: &dyn Fn(&Section) -> Option<u32>,
        new_index: &dyn Fn(u32) -> Option<u32>,
        forms: Forms,
        sid: Option<&str>,
        kind: PortKind,
        deep: bool,
    ) {
        if let Some(n) = index_of(s).filter(|&i| i >= 1).and_then(new_index) {
            let [sid_key, _, port_key] = forms;
            match s.prop(sid_key) {
                Some(_) => {
                    let ep = format!("{}#{}:{n}", sid.unwrap_or_default(), kind.token());
                    let bare = s.was_quoted(sid_key) == Some(false);
                    s.set_prop(sid_key, &ep, bare);
                }
                None => s.set_prop(port_key, &n.to_string(), true),
            }
        }
        if deep {
            for b in s.sections_mut().filter(|b| b.tag == "Branch") {
                renumber(b, index_of, new_index, forms, sid, kind, true);
            }
        }
    }
    for line in sys.sections_mut().filter(|l| l.tag == "Line") {
        renumber(
            line,
            &index_of,
            &new_index,
            forms,
            sid.as_deref(),
            remap.kind,
            remap.kind != PortKind::Out,
        );
    }
    Ok(())
}

/// A port on a block of the edited system, which lines may name either by
/// block name and port (`SrcBlock`/`SrcPort`) or by SID (`Src "12#out:1"`).
struct Port<'a> {
    name: &'a str,
    sid: Option<String>,
    port: PortRef,
    default_kind: PortKind,
}

/// The SID, name and port keys of an endpoint.
type Forms = [&'static str; 3];
const SRC_FORMS: Forms = ["Src", "SrcBlock", "SrcPort"];
const DST_FORMS: Forms = ["Dst", "DstBlock", "DstPort"];

impl<'a> Port<'a> {
    fn new(sys: &Section, name: &'a str, port: PortRef, default_kind: PortKind) -> Self {
        let sid = sys
            .sections()
            .find(|b| b.tag == "Block" && b.prop("Name").as_deref() == Some(name))
            .and_then(|b| b.prop("SID"));
        Port {
            name,
            sid,
            port,
            default_kind,
        }
    }

    /// Whether `s`'s endpoint written under `forms` is this port; read as
    /// the importer reads it.
    fn is_at(&self, s: &Section, [sid_key, block_key, port_key]: Forms) -> bool {
        if let Some(v) = s.prop(sid_key) {
            return parse_endpoint(&v, self.default_kind)
                .is_some_and(|(sid, port)| Some(sid) == self.sid.as_deref() && port == self.port);
        }
        s.prop(block_key).as_deref() == Some(self.name)
            && parse_port(
                s.prop(port_key).as_deref().unwrap_or("1"),
                self.default_kind,
            ) == Some(self.port)
    }

    /// Write this port under the name forms of `forms`.
    fn write(&self, s: &mut Section, [_, block_key, port_key]: Forms) {
        s.set_prop(block_key, self.name, false);
        let port = match self.port.kind {
            PortKind::In | PortKind::Out => self.port.index.to_string(),
            kind => kind.token().to_string(),
        };
        s.set_prop(port_key, &port, true);
    }
}

fn new_section(tag: &str, indent: &str, eol: &str) -> Section {
    Section {
        tag: tag.into(),
        header: format!("{indent}{tag} {{{eol}"),
        items: Vec::new(),
        footer: format!("{indent}}}{eol}"),
    }
}

/// Indentation for a new child section of `s`.
fn child_indent(s: &Section) -> String {
    s.items
        .iter()
        .find_map(|i| match i {
            Item::Section(c) => Some(indent_of(&c.header).to_string()),
            Item::Prop { lines, .. } => Some(indent_of(&lines[0]).to_string()),
            Item::Raw(_) => None,
        })
        .unwrap_or_else(|| format!("{}  ", indent_of(&s.header)))
}

/// Insert `section` after the last child section tagged `after`, or at the
/// end.
fn insert_after(s: &mut Section, after: &[&str], section: Section) {
    let at = s
        .items
        .iter()
        .rposition(|i| matches!(i, Item::Section(c) if after.contains(&c.tag.as_str())))
        .map_or(s.items.len(), |i| i + 1);
    s.items.insert(at, Item::Section(section));
}

fn add_block(sys: &mut Section, block: &Block) {
    let mut b = new_section("Block", &child_indent(sys), sys.eol());
    b.set_prop("BlockType", &block.block_type, true);
    b.set_prop("Name", &block.name, false);
    b.set_prop("SID", &block.id.0, false);
    b.set_prop("Ports", &format_ports(&block.ports), true);
    b.set_prop("Position", &format_rect(&block.position), true);
    for (key, value) in &block.parameters {
        b.set_prop(key, value, false);
    }
    insert_after(sys, &["Block"], b);
}

/// Mirror of the IR's connect: branch the source's existing line (its
/// destination becoming the first branch), or start a new line.
fn connect(sys: &mut Section, src: &Port, dst: &Port) {
    let eol = sys.eol().to_string();
    let line = sys
        .sections_mut()
        .find(|l| l.tag == "Line" && src.is_at(l, SRC_FORMS));
    let Some(line) = line else {
        let mut line = new_section("Line", &child_indent(sys), &eol);
        src.write(&mut line, SRC_FORMS);
        dst.write(&mut line, DST_FORMS);
        insert_after(sys, &["Line", "Block"], line);
        return;
    };
    let indent = child_indent(line);
    if DST_FORMS[..2].iter().any(|k| line.prop(k).is_some()) {
        let mut old = new_section("Branch", &indent, &eol);
        for key in DST_FORMS {
            if let Some(value) = line.prop(key) {
                old.set_prop(key, &value, line.was_quoted(key) == Some(false));
                line.remove_prop(key);
            }
        }
        let at = line
            .items
            .iter()
            .position(|i| matches!(i, Item::Section(b) if b.tag == "Branch"))
            .unwrap_or(line.items.len());
        line.items.insert(at, Item::Section(old));
    }
    let mut new = new_section("Branch", &indent, &eol);
    dst.write(&mut new, DST_FORMS);
    line.items.push(Item::Section(new));
}

/// Whether `hit` matches the line or branch `s` or any branch below it.
fn reaches(s: &Section, hit: &dyn Fn(&Section, bool) -> bool, is_line: bool) -> bool {
    hit(s, is_line)
        || s.sections()
            .any(|b| b.tag == "Branch" && reaches(b, hit, false))
}

pub(super) fn apply(text: &str, edits: &[Resolved]) -> Result<String, ImportError> {
    let mut file = parse(text)?;
    for resolved in edits {
        apply_edit(&mut file, resolved)?;
        if let Some(boundary) = &resolved.boundary {
            apply_boundary(&mut file, boundary)?;
        }
    }
    Ok(file.to_text())
}

fn set_route_points(section: &mut Section, points: &super::RoutePoints) -> Result<(), ImportError> {
    if section.sections().filter(|b| b.tag == "Branch").count() != points.branches.len() {
        return Err(ImportError::Edit(
            "serialized route topology differs from imported model".into(),
        ));
    }
    if points.value == "[]" {
        section.remove_prop("Points");
    } else {
        // A bare matrix, as Simulink writes it; the value is numbers only.
        section.set_prop("Points", &points.value, true);
    }
    for (branch, p) in section
        .sections_mut()
        .filter(|b| b.tag == "Branch")
        .zip(&points.branches)
    {
        set_route_points(branch, p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "Model {\n  Name\t\"m\"\n  System {\n    Name\t\"m\"\n    Block {\n      BlockType\tGain\n      Name\t\"g\"\n      Position\t[10, 10, 40, 40]\n      Gain\t\"2\"\n    }\n    Block {\n      BlockType\tOutport\n      Name\t\"out\"\n      Position\t[100, 10, 130, 40]\n    }\n    Line {\n      SrcBlock\t\"g\"\n      SrcPort\t1\n      Points\t[20, 0]\n      DstBlock\t\"out\"\n      DstPort\t1\n    }\n  }\n}\n";

    /// An edit to block `name` (id `x`) in the root system.
    fn at_root(edit: Edit, name: &str) -> Resolved {
        Resolved {
            edit,
            system: vec![],
            names: [("x".into(), name.into())].into(),
            added: None,
            route: None,
            hierarchy: None,
            expansion: None,
            source_line_count: 0,
            ports: None,
            renumbered: Vec::new(),
            boundary: None,
        }
    }

    #[test]
    fn unmodified_roundtrip_is_identical() {
        assert_eq!(parse(SRC).unwrap().to_text(), SRC);
    }

    #[test]
    fn move_rename_param_delete() {
        let mv = Edit::MoveBlock {
            system: vec![],
            id: "x".into(),
            position: Rect::new(20.0, 20.0, 50.0, 50.0),
        };
        let out = apply(SRC, &[at_root(mv, "g")]).unwrap();
        assert!(out.contains("Position\t[20, 20, 50, 50]"));
        assert!(!out.contains("Points"));

        let rn = Edit::RenameBlock {
            system: vec![],
            id: "x".into(),
            name: "gain \"one\"".into(),
        };
        let out = apply(SRC, &[at_root(rn, "g")]).unwrap();
        assert!(out.contains("Name\t\"gain \\\"one\\\"\""));
        assert!(out.contains("SrcBlock\t\"gain \\\"one\\\"\""));

        let sp = Edit::SetParameter {
            system: vec![],
            id: "x".into(),
            name: "Gain".into(),
            value: "K*3".into(),
        };
        let out = apply(SRC, &[at_root(sp, "g")]).unwrap();
        assert!(out.contains("Gain\t\"K*3\""));

        let del = Edit::DeleteBlock {
            system: vec![],
            id: "x".into(),
            disconnect: unlinked_model::edit::DisconnectPolicy::Disconnect,
        };
        let out = apply(SRC, &[at_root(del, "out")]).unwrap();
        assert!(!out.contains("\"out\""));
        assert!(
            !out.contains("Line {"),
            "line left without destination is removed:\n{out}"
        );
    }

    #[test]
    fn crlf_and_opc_tail_are_preserved() {
        let src = "Model {\r\n  System {\r\n    Block {\r\n      Name\t\"g\"\r\n      Gain\t\"2\"\r\n    }\r\n  }\r\n}\r\n__MWOPC_PACKAGE_BEGIN__ R2020a\r\nbinary\ttail";
        assert_eq!(parse(src).unwrap().to_text(), src);
        let sp = Edit::SetParameter {
            system: vec![],
            id: "x".into(),
            name: "Gain".into(),
            value: "5".into(),
        };
        let out = apply(src, &[at_root(sp, "g")]).unwrap();
        assert_eq!(out, src.replace("Gain\t\"2\"", "Gain\t\"5\""));
    }

    #[test]
    fn bare_values_cannot_inject_structure() {
        let src =
            "Model {\n  System {\n    Block {\n      Name\t\"g\"\n      Inputs\t2\n    }\n  }\n}\n";
        let sp = |value: &str| {
            let e = Edit::SetParameter {
                system: vec![],
                id: "x".into(),
                name: "Inputs".into(),
                value: value.into(),
            };
            apply(src, &[at_root(e, "g")]).unwrap()
        };
        assert!(sp("3").contains("Inputs\t3\n"));
        let out = sp("3\n    }\n    Block {\n      Name\t\"evil\"");
        assert!(out.contains("Inputs\t\"3\\n    }"), "{out}");
        let reparsed = parse(&out).unwrap().to_text();
        assert_eq!(reparsed, out);
        assert_eq!(out.lines().filter(|l| l.trim() == "Block {").count(), 1);
    }

    #[test]
    fn old_style_mask_values() {
        let src = "Model {\n  System {\n    Block {\n      Name\t\"s\"\n      MaskVariables\t\"a=@1;b=@2;\"\n      MaskValueString\t\"1|2\"\n    }\n  }\n}\n";
        let sp = Edit::SetParameter {
            system: vec![],
            id: "x".into(),
            name: "b".into(),
            value: "7".into(),
        };
        let out = apply(src, &[at_root(sp, "s")]).unwrap();
        assert!(out.contains("MaskValueString\t\"1|7\""));
    }
}
