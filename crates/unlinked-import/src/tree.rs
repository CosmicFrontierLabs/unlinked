//! Format-neutral element tree.
//!
//! Both SLX XML and MDL text reduce to the same shape: a tagged node with
//! named scalar properties and nested child nodes. SLX `<P Name="X">v</P>`
//! elements and MDL `X v` lines both become properties; XML attributes are
//! kept separately so `Block BlockType=".."` and MDL `BlockType ..` can be
//! looked up the same way through [`Node::get`].

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub props: Vec<(String, String)>,
    /// Character data directly inside the element (XML only).
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(tag: impl Into<String>) -> Self {
        Node {
            tag: tag.into(),
            ..Default::default()
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn prop(&self, name: &str) -> Option<&str> {
        self.props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Attribute or property, attribute first.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attr(name).or_else(|| self.prop(name))
    }

    pub fn child(&self, tag: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.tag == tag)
    }

    pub fn children_named<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.tag == tag)
    }

    /// Depth-first search for the first descendant (or self) matching `pred`.
    pub fn find(&self, pred: &dyn Fn(&Node) -> bool) -> Option<&Node> {
        if pred(self) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(pred))
    }
}
