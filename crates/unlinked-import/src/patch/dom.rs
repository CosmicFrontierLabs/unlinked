//! A lossless XML DOM: parsing then serializing an unmodified document
//! reproduces it byte for byte (text and attribute values keep their
//! original escaping), so edits touch only the nodes they change.

use crate::{ImportError, MAX_DEPTH, MAX_NODES};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Debug, Clone, PartialEq)]
pub enum XNode {
    Element(XElem),
    /// Character data exactly as written, entity references included.
    Text(String),
    /// Declarations, comments, CDATA and processing instructions, verbatim.
    Raw(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct XElem {
    pub name: String,
    /// Attribute values exactly as written (still escaped).
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XNode>,
    /// Written as `<name/>`.
    pub empty: bool,
}

pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;").replace('\n', "&#xA;")
}

/// Decode the entity and character references in raw text or attribute
/// values.
pub fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let name = &tail[1..end];
        let decoded = match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => name
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

impl XElem {
    pub fn new(name: &str) -> Self {
        XElem {
            name: name.to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
            empty: false,
        }
    }

    pub fn attr(&self, name: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| unescape(v))
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        let v = escape_attr(value);
        match self.attrs.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = v,
            None => self.attrs.push((name.to_string(), v)),
        }
    }

    /// Concatenated, unescaped character data of direct text children.
    pub fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|c| match c {
                XNode::Text(t) => Some(unescape(t)),
                _ => None,
            })
            .collect()
    }

    pub fn set_text(&mut self, value: &str) {
        self.children.retain(|c| !matches!(c, XNode::Text(_)));
        self.children.insert(0, XNode::Text(escape_text(value)));
        self.empty = false;
    }

    pub fn elements(&self) -> impl Iterator<Item = &XElem> {
        self.children.iter().filter_map(|c| match c {
            XNode::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn elements_mut(&mut self) -> impl Iterator<Item = &mut XElem> {
        self.children.iter_mut().filter_map(|c| match c {
            XNode::Element(e) => Some(e),
            _ => None,
        })
    }

    /// The direct `<P Name="name">` child.
    pub fn prop_mut(&mut self, name: &str) -> Option<&mut XElem> {
        self.elements_mut()
            .find(|e| e.name == "P" && e.attr("Name").as_deref() == Some(name))
    }

    pub fn prop(&self, name: &str) -> Option<String> {
        self.elements()
            .find(|e| e.name == "P" && e.attr("Name").as_deref() == Some(name))
            .map(XElem::text)
    }

    /// Append `<P Name="name">value</P>`, matching the indentation of the
    /// element's existing children.
    pub fn push_prop(&mut self, name: &str, value: &str) {
        let indent = self
            .children
            .iter()
            .find_map(|c| match c {
                XNode::Text(t) if t.contains('\n') => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mut p = XElem::new("P");
        p.set_attr("Name", name);
        p.set_text(value);
        // Insert before the trailing whitespace that precedes the end tag.
        let at = match self.children.last() {
            Some(XNode::Text(t)) if t.trim().is_empty() => self.children.len() - 1,
            _ => self.children.len(),
        };
        self.children.insert(at, XNode::Element(p));
        if !indent.is_empty() {
            self.children.insert(at, XNode::Text(indent));
        }
        self.empty = false;
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
        if self.empty && self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for c in &self.children {
            write_node(c, out);
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

fn write_node(node: &XNode, out: &mut String) {
    match node {
        XNode::Element(e) => e.write(out),
        XNode::Text(t) | XNode::Raw(t) => out.push_str(t),
    }
}

pub struct Document {
    pub nodes: Vec<XNode>,
}

impl Document {
    pub fn root_mut(&mut self) -> Option<&mut XElem> {
        self.nodes.iter_mut().find_map(|n| match n {
            XNode::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        for n in &self.nodes {
            write_node(n, &mut out);
        }
        out
    }
}

fn start(e: &BytesStart) -> Result<XElem, ImportError> {
    let err = |e: quick_xml::events::attributes::AttrError| ImportError::Xml(e.to_string());
    let mut el = XElem::new(e.name().as_ref());
    for a in e.attributes() {
        let a = a.map_err(err)?;
        el.attrs
            .push((a.key.as_ref().to_string(), a.value.to_string()));
    }
    Ok(el)
}

pub fn parse(text: &str) -> Result<Document, ImportError> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<XElem> = Vec::new();
    let mut top: Vec<XNode> = Vec::new();
    let mut budget = MAX_NODES;
    let push = |stack: &mut Vec<XElem>, top: &mut Vec<XNode>, n: XNode| match stack.last_mut() {
        Some(parent) => parent.children.push(n),
        None => top.push(n),
    };
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|e| ImportError::Xml(format!("at byte {}: {e}", reader.error_position())))?;
        // The exact source of the event, for everything kept verbatim.
        let raw = || text[before..reader.buffer_position() as usize].to_string();
        if !matches!(event, Event::Eof) {
            budget = budget
                .checked_sub(1)
                .ok_or_else(|| ImportError::Xml("too many XML nodes".into()))?;
        }
        match event {
            Event::Start(e) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(ImportError::Xml("nesting too deep".into()));
                }
                stack.push(start(&e)?);
            }
            Event::Empty(e) => {
                let mut el = start(&e)?;
                el.empty = true;
                push(&mut stack, &mut top, XNode::Element(el));
            }
            Event::End(_) => {
                let el = stack
                    .pop()
                    .ok_or_else(|| ImportError::Xml("unbalanced end tag".into()))?;
                push(&mut stack, &mut top, XNode::Element(el));
            }
            Event::Text(_) | Event::GeneralRef(_) => {
                let raw = raw();
                match stack.last_mut().and_then(|p| p.children.last_mut()) {
                    Some(XNode::Text(t)) => t.push_str(&raw),
                    _ => push(&mut stack, &mut top, XNode::Text(raw)),
                }
            }
            Event::CData(_)
            | Event::Comment(_)
            | Event::Decl(_)
            | Event::PI(_)
            | Event::DocType(_) => push(&mut stack, &mut top, XNode::Raw(raw())),
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(ImportError::Xml("unexpected end of document".into()));
    }
    Ok(Document { nodes: top })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_is_byte_identical() {
        let src = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<A x=\"1&amp;2\">\n  <P Name=\"N\">a&#xA;b &lt;c&gt;</P>\n  <!-- note -->\n  <E/>\n</A>\n";
        let doc = parse(src).unwrap();
        assert_eq!(doc.to_xml(), src);
    }

    #[test]
    fn text_and_attrs_unescape_and_escape() {
        let mut doc = parse("<A Name=\"x&#xA;y\"><P Name=\"V\">1 &lt; 2</P></A>").unwrap();
        let root = doc.root_mut().unwrap();
        assert_eq!(root.attr("Name").unwrap(), "x\ny");
        assert_eq!(root.prop("V").unwrap(), "1 < 2");
        root.set_attr("Name", "a\"b");
        root.prop_mut("V").unwrap().set_text("3 > 2 & 1");
        root.push_prop("New", "<v>");
        assert_eq!(
            doc.to_xml(),
            "<A Name=\"a&quot;b\"><P Name=\"V\">3 &gt; 2 &amp; 1</P><P Name=\"New\">&lt;v&gt;</P></A>"
        );
    }

    #[test]
    fn push_prop_keeps_indentation() {
        let mut doc = parse("<B>\n  <P Name=\"a\">1</P>\n</B>").unwrap();
        doc.root_mut().unwrap().push_prop("b", "2");
        assert_eq!(
            doc.to_xml(),
            "<B>\n  <P Name=\"a\">1</P>\n  <P Name=\"b\">2</P>\n</B>"
        );
    }

    #[test]
    fn unescape_handles_references() {
        assert_eq!(unescape("a&amp;b&#65;&#x42;&bogus;"), "a&bAB&bogus;");
    }
}
