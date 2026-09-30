//! Apply edits to MDL text. The file is held as a tree of sections whose
//! lines are kept verbatim, so untouched content is reproduced exactly.
//! Blocks are found by the containing system's path and the block name
//! (MDL lines refer to blocks by name).

use crate::{ImportError, MAX_DEPTH, MAX_NODES};
use unlinked_model::edit::Edit;
use unlinked_model::Rect;

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

fn prune(s: &mut Section, name: &str, sid: Option<&str>, is_line: bool) -> bool {
    let keys: &[&str] = if is_line {
        &DST_KEYS
    } else {
        &["DstBlock", "Dst", "SrcBlock", "Src"]
    };
    if endpoint_is(s, keys, name, sid) {
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
        Item::Section(b) if b.tag == "Branch" => prune(b, name, sid, false),
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

/// `name` is the block's current name (the edit refers to it by id).
fn apply_edit(file: &mut MdlFile, edit: &Edit, name: &str) -> Result<(), ImportError> {
    let path = edit.system();
    let sys = file
        .system_mut(path)
        .ok_or_else(|| ImportError::Mdl(format!("no system at {path:?}")))?;
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
        Edit::SetParameter {
            name: key, value, ..
        } => {
            let b = block_at(sys, i);
            if !set_mask_parameter(b, key, value) {
                let bare = b.was_quoted(key) == Some(false) && is_bare_token(value);
                b.set_prop(key, value, bare);
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
            sys.items.retain_mut(|item| match item {
                Item::Section(l) if l.tag == "Line" && touches(l, name, sid) => {
                    !endpoint_is(l, &SRC_KEYS, name, sid) && prune(l, name, sid, true)
                }
                _ => true,
            });
        }
    }
    Ok(())
}

pub fn apply(text: &str, edits: &[(Edit, String)]) -> Result<String, ImportError> {
    let mut file = parse(text)?;
    for (edit, name) in edits {
        apply_edit(&mut file, edit, name)?;
    }
    Ok(file.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "Model {\n  Name\t\"m\"\n  System {\n    Name\t\"m\"\n    Block {\n      BlockType\tGain\n      Name\t\"g\"\n      Position\t[10, 10, 40, 40]\n      Gain\t\"2\"\n    }\n    Block {\n      BlockType\tOutport\n      Name\t\"out\"\n      Position\t[100, 10, 130, 40]\n    }\n    Line {\n      SrcBlock\t\"g\"\n      SrcPort\t1\n      Points\t[20, 0]\n      DstBlock\t\"out\"\n      DstPort\t1\n    }\n  }\n}\n";

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
        let out = apply(SRC, &[(mv, "g".into())]).unwrap();
        assert!(out.contains("Position\t[20, 20, 50, 50]"));
        assert!(!out.contains("Points"));

        let rn = Edit::RenameBlock {
            system: vec![],
            id: "x".into(),
            name: "gain \"one\"".into(),
        };
        let out = apply(SRC, &[(rn, "g".into())]).unwrap();
        assert!(out.contains("Name\t\"gain \\\"one\\\"\""));
        assert!(out.contains("SrcBlock\t\"gain \\\"one\\\"\""));

        let sp = Edit::SetParameter {
            system: vec![],
            id: "x".into(),
            name: "Gain".into(),
            value: "K*3".into(),
        };
        let out = apply(SRC, &[(sp, "g".into())]).unwrap();
        assert!(out.contains("Gain\t\"K*3\""));

        let del = Edit::DeleteBlock {
            system: vec![],
            id: "x".into(),
        };
        let out = apply(SRC, &[(del, "out".into())]).unwrap();
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
        let out = apply(src, &[(sp, "g".into())]).unwrap();
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
            apply(src, &[(e, "g".into())]).unwrap()
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
        let out = apply(src, &[(sp, "s".into())]).unwrap();
        assert!(out.contains("MaskValueString\t\"1|7\""));
    }
}
