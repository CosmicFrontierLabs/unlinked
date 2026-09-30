//! Import Simulink `.slx` and `.mdl` files into the [`unlinked_model`] IR.
//!
//! ```no_run
//! let bytes = std::fs::read("model.slx").unwrap();
//! let model = unlinked_import::import("model.slx", &bytes).unwrap();
//! println!("{} blocks", model.block_count());
//! ```

mod convert;
pub mod mdl;
pub mod slx;
pub mod stateflow;
pub mod tree;

use convert::{read_type_defaults, sim_config, Converter, TypeDefaults};
use std::collections::BTreeMap;
use tree::Node;
use unlinked_model::{Model, SourceFormat};

/// Upper bound on bytes read from one model: the uncompressed total of an
/// SLX archive's parts, or the size of an MDL file. Guards against zip bombs.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Maximum element nesting accepted by the parsers. Real models stay far
/// below this; the limit keeps recursive tree walks and drops off the edge
/// of the stack.
pub const MAX_DEPTH: usize = 256;

/// Maximum number of parsed elements (XML elements plus attributes, or MDL
/// lines) per model, bounding memory for very wide trees. The largest
/// corpus model uses well under a tenth of this.
pub const MAX_NODES: usize = 2_000_000;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("not a Simulink model: {0}")]
    NotAModel(String),
    #[error("invalid SLX archive: {0}")]
    Zip(String),
    #[error("SLX archive is missing part {0}")]
    MissingPart(String),
    #[error("model exceeds the size limit")]
    TooLarge,
    #[error("invalid XML: {0}")]
    Xml(String),
    #[error("invalid MDL: {0}")]
    Mdl(String),
}

/// Import a model, detecting the format from the content (zip magic ⇒ SLX).
/// `filename` supplies the model name when the file does not record one.
pub fn import(filename: &str, bytes: &[u8]) -> Result<Model, ImportError> {
    if bytes.starts_with(b"PK\x03\x04") {
        import_slx(filename, bytes)
    } else {
        import_mdl(filename, &decode_text(bytes))
    }
}

/// Decode MDL bytes: UTF-8 when valid, otherwise windows-1252, which is what
/// older files declare in `SavedCharacterEncoding`.
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

/// Map one windows-1252 byte to its character. 0x80..=0x9F differ from
/// Latin-1; the five bytes undefined in windows-1252 map to U+FFFD.
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž',
        '\u{FFFD}', '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{FFFD}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

fn stem(filename: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    match base.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s.to_string(),
        _ => base.to_string(),
    }
}

pub fn import_slx(filename: &str, bytes: &[u8]) -> Result<Model, ImportError> {
    let mut pkg = slx::SlxPackage::open(bytes)?;
    let doc = pkg.block_diagram()?;
    let model_node = doc
        .children
        .iter()
        .find(|c| c.tag == "Model" || c.tag == "Library" || c.tag == "Subsystem")
        .ok_or_else(|| {
            ImportError::NotAModel("blockdiagram.xml has no <Model> or <Library>".into())
        })?;

    let mut defaults = TypeDefaults::new();
    read_type_defaults(&doc, &mut defaults);
    let bd_defaults = if pkg.has("simulink/bddefaults.xml") {
        Some(pkg.read_xml("simulink/bddefaults.xml")?)
    } else {
        None
    };
    if let Some(d) = &bd_defaults {
        read_type_defaults(d, &mut defaults);
    }

    let config_set = if pkg.has("simulink/configSet0.xml") {
        Some(pkg.read_xml("simulink/configSet0.xml")?)
    } else {
        None
    };
    let mut trees: Vec<&Node> = Vec::new();
    trees.extend(config_set.as_ref());
    trees.push(&doc);
    let config = sim_config(&trees, model_node);

    let simulink_version = if pkg.has("metadata/coreProperties.xml") {
        let props = pkg.read_xml("metadata/coreProperties.xml")?;
        props
            .find(&|n| n.tag == "cp:version" || n.tag == "version")
            .map(|n| n.text.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    let name = model_node
        .get("Name")
        .map(str::to_string)
        .unwrap_or_else(|| stem(filename));
    let system_node = model_node
        .child("System")
        .ok_or_else(|| ImportError::NotAModel("model has no root <System>".into()))?;
    let root = Converter::new(&defaults).system(system_node, &name)?;
    let mut charts = stateflow::read_slx(&mut pkg)?;
    stateflow::relativize(&mut charts, &name, &root);

    Ok(Model {
        name,
        source: SourceFormat::Slx,
        simulink_version,
        config,
        root,
        workspace: BTreeMap::new(),
        charts,
    })
}

pub fn import_mdl(filename: &str, text: &str) -> Result<Model, ImportError> {
    if text.len() as u64 > MAX_UNCOMPRESSED_BYTES {
        return Err(ImportError::TooLarge);
    }
    let sections = mdl::parse(text)?;
    let model_node = sections
        .iter()
        .find(|s| s.tag == "Model" || s.tag == "Library")
        .ok_or_else(|| ImportError::NotAModel("no Model or Library section".into()))?;

    let mut defaults = TypeDefaults::new();
    read_type_defaults(model_node, &mut defaults);

    let config = sim_config(&[model_node], model_node);
    let name = model_node
        .prop("Name")
        .map(str::to_string)
        .unwrap_or_else(|| stem(filename));
    let system_node = model_node
        .child("System")
        .ok_or_else(|| ImportError::NotAModel("model has no root System".into()))?;
    let root = Converter::new(&defaults).system(system_node, &name)?;
    let mut charts = sections
        .iter()
        .filter(|s| s.tag == "Stateflow")
        .flat_map(stateflow::mdl_charts)
        .collect::<Vec<_>>();
    stateflow::relativize(&mut charts, &name, &root);

    Ok(Model {
        name,
        source: SourceFormat::Mdl,
        simulink_version: model_node.prop("Version").map(str::to_string),
        config,
        root,
        workspace: BTreeMap::new(),
        charts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_mdl_nesting_is_rejected() {
        let depth = 100_000;
        let text = "Model {\n".repeat(depth) + &"}\n".repeat(depth);
        assert!(matches!(
            import("deep.mdl", text.as_bytes()),
            Err(ImportError::Mdl(_))
        ));
    }

    #[test]
    fn deep_xml_nesting_is_rejected() {
        let depth = 100_000;
        let text = "<A>".repeat(depth) + &"</A>".repeat(depth);
        assert!(slx::parse_xml(&text, &mut MAX_NODES.clone()).is_err());
    }

    #[test]
    fn wide_xml_exhausts_node_budget() {
        let text = format!("<A>{}</A>", "<B/>".repeat(1000));
        assert!(slx::parse_xml(&text, &mut 500).is_err());
        assert!(slx::parse_xml(&text, &mut 2000).is_ok());
    }

    #[test]
    fn windows_1252_text_decodes() {
        assert_eq!(decode_text(b"\x80 \x93q\x94 \xe9"), "€ “q” é");
    }

    #[test]
    fn huge_port_numbers_are_ignored() {
        let text = r#"Model {
  Name "m"
  System {
    Block {
      BlockType Constant
      Name "c"
      Ports [0, 4294967295]
      Position [0, 0, 10, 10]
    }
    Block {
      BlockType Terminator
      Name "t"
      Position [50, 0, 60, 10]
    }
    Line {
      SrcBlock "c"
      SrcPort 4294967295
      DstBlock "t"
      DstPort 1
    }
  }
}
"#;
        let model = import("m.mdl", text.as_bytes()).unwrap();
        assert!(model.root.blocks[0].ports.outputs <= 1024);
        assert!(model.root.connections().is_empty());
    }
}
