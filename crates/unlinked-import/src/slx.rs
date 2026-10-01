//! Reader for `.slx` packages (Open Packaging Convention zip archives).
//!
//! The model lives in `simulink/blockdiagram.xml`. Releases from R2019b on
//! split each diagram level into `simulink/systems/system_<id>.xml` and leave
//! `<System Ref="system_<id>"/>` placeholders, which are resolved here so the
//! converter always sees a fully inlined tree.

use crate::tree::Node;
use crate::{ImportError, MAX_DEPTH};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::io::{Cursor, Read};

pub struct SlxPackage {
    archive: zip::ZipArchive<Cursor<Vec<u8>>>,
    read_so_far: u64,
    /// Elements and attributes still allowed across all parsed parts.
    node_budget: usize,
}

impl SlxPackage {
    pub fn open(bytes: &[u8]) -> Result<Self, ImportError> {
        let archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec()))
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        Ok(SlxPackage {
            archive,
            read_so_far: 0,
            node_budget: crate::MAX_NODES,
        })
    }

    pub fn has(&self, name: &str) -> bool {
        self.archive.index_for_name(name).is_some()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.archive.file_names()
    }

    pub fn read_string(&mut self, name: &str) -> Result<String, ImportError> {
        let mut file = self
            .archive
            .by_name(name)
            .map_err(|_| ImportError::MissingPart(name.to_string()))?;
        let budget = crate::MAX_UNCOMPRESSED_BYTES.saturating_sub(self.read_so_far);
        if file.size() > budget {
            return Err(ImportError::TooLarge);
        }
        let mut buf = Vec::with_capacity(file.size() as usize);
        (&mut file)
            .take(budget + 1)
            .read_to_end(&mut buf)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        self.read_so_far += buf.len() as u64;
        if self.read_so_far > crate::MAX_UNCOMPRESSED_BYTES {
            return Err(ImportError::TooLarge);
        }
        String::from_utf8(buf).map_err(|_| ImportError::Xml(format!("{name} is not UTF-8")))
    }

    /// Parse a part as XML, returning its document element.
    pub fn read_xml(&mut self, name: &str) -> Result<Node, ImportError> {
        let text = self.read_string(name)?;
        parse_xml(&text, &mut self.node_budget)
            .map_err(|e| ImportError::Xml(format!("{name}: {e}")))
    }

    /// Parse `blockdiagram.xml` and inline every split-out system part.
    pub fn block_diagram(&mut self) -> Result<Node, ImportError> {
        let mut root = self.read_xml("simulink/blockdiagram.xml")?;
        self.inline_system_refs(&mut root, 0)?;
        Ok(root)
    }

    fn inline_system_refs(&mut self, node: &mut Node, depth: usize) -> Result<(), ImportError> {
        if depth > MAX_DEPTH {
            return Err(ImportError::Xml("system nesting too deep".into()));
        }
        for child in node.children.iter_mut() {
            if child.tag == "System" {
                if let Some(r) = child.attr("Ref").map(str::to_string) {
                    let part = format!("simulink/systems/{r}.xml");
                    let mut sys = self.read_xml(&part)?;
                    if sys.tag != "System" {
                        return Err(ImportError::Xml(format!("{part}: expected <System>")));
                    }
                    sys.attrs.retain(|(k, _)| k != "Ref");
                    *child = sys;
                }
            }
            self.inline_system_refs(child, depth + 1)?;
        }
        Ok(())
    }
}

/// Parse an XML document into a [`Node`] tree. `<P Name="k">v</P>` elements
/// become properties of their parent; everything else becomes a child node.
pub fn parse_xml(text: &str, budget: &mut usize) -> Result<Node, String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);

    // Each frame is (node, is_property_element).
    let mut stack: Vec<(Node, bool)> = vec![(Node::new(""), false)];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let node = element(&e, budget)?;
                let is_prop = node.tag == "P" && node.attr("Name").is_some();
                if stack.len() > MAX_DEPTH {
                    return Err("nesting too deep".into());
                }
                stack.push((node, is_prop));
            }
            Ok(Event::Empty(e)) => {
                let node = element(&e, budget)?;
                close(&mut stack, node, node_is_prop(&e));
            }
            Ok(Event::End(_)) => {
                if stack.len() < 2 {
                    return Err("unbalanced end tag".into());
                }
                let (node, is_prop) = stack.pop().unwrap();
                close(&mut stack, node, is_prop);
            }
            Ok(Event::Text(t)) => stack
                .last_mut()
                .unwrap()
                .0
                .text
                .push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => stack.last_mut().unwrap().0.text.push_str(&t.into_inner()),
            Ok(Event::GeneralRef(r)) => {
                let text = &mut stack.last_mut().unwrap().0.text;
                if let Some(c) = r.resolve_char_ref().map_err(|e| e.to_string())? {
                    text.push(c);
                } else {
                    let name = r.xml10_content();
                    text.push_str(match name.as_ref() {
                        "lt" => "<",
                        "gt" => ">",
                        "amp" => "&",
                        "apos" => "'",
                        "quot" => "\"",
                        other => return Err(format!("unknown entity &{other};")),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("at byte {}: {e}", reader.error_position())),
        }
    }
    if stack.len() != 1 {
        return Err("unexpected end of document".into());
    }
    let (doc, _) = stack.pop().unwrap();
    doc.children
        .into_iter()
        .next()
        .ok_or_else(|| "empty document".to_string())
}

fn spend(budget: &mut usize) -> Result<(), String> {
    *budget = budget
        .checked_sub(1)
        .ok_or_else(|| "too many XML elements".to_string())?;
    Ok(())
}

fn node_is_prop(e: &BytesStart) -> bool {
    e.name().as_ref() == "P"
}

fn element(e: &BytesStart, budget: &mut usize) -> Result<Node, String> {
    spend(budget)?;
    let mut node = Node::new(e.name().as_ref().to_string());
    for attr in e.attributes() {
        spend(budget)?;
        let attr = attr.map_err(|e| e.to_string())?;
        let key = attr.key.as_ref().to_string();
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?
            .into_owned();
        node.attrs.push((key, value));
    }
    Ok(node)
}

fn close(stack: &mut [(Node, bool)], node: Node, is_prop: bool) {
    let parent = &mut stack.last_mut().unwrap().0;
    if is_prop {
        if let Some(name) = node.attr("Name") {
            parent.props.push((name.to_string(), node.text));
            return;
        }
    }
    parent.children.push(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_become_properties() {
        let doc = parse_xml(
            r#"<?xml version="1.0"?>
<ModelInformation>
  <Model>
    <System>
      <Block BlockType="Reference" Name="Compare&#xA;To Zero" SID="10">
        <P Name="Position">[390, 410, 450, 440]</P>
        <P Name="relop">&lt;=</P>
        <InstanceData><P Name="ZeroCross">on</P></InstanceData>
      </Block>
    </System>
  </Model>
</ModelInformation>"#,
            &mut 1000,
        )
        .unwrap();
        assert_eq!(doc.tag, "ModelInformation");
        let block = &doc
            .child("Model")
            .unwrap()
            .child("System")
            .unwrap()
            .children[0];
        assert_eq!(block.get("Name"), Some("Compare\nTo Zero"));
        assert_eq!(block.get("SID"), Some("10"));
        assert_eq!(block.prop("Position"), Some("[390, 410, 450, 440]"));
        assert_eq!(block.prop("relop"), Some("<="));
        assert_eq!(
            block.child("InstanceData").unwrap().prop("ZeroCross"),
            Some("on")
        );
    }

    #[test]
    fn empty_p_is_empty_property() {
        let doc = parse_xml(
            r#"<A><P Name="CreateCallback" Class="char"/></A>"#,
            &mut 1000,
        )
        .unwrap();
        assert_eq!(doc.prop("CreateCallback"), Some(""));
    }
}
