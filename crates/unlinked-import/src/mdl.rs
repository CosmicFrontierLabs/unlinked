//! Tokenizer for the legacy `.mdl` text format.
//!
//! The format is line oriented:
//!
//! ```text
//! Model {
//!   Name      "example"
//!   Block {
//!     BlockType Gain
//!     Name      "proportional\ngain"
//!     Position  [95, 92, 145, 138]
//!   }
//! }
//! ```
//!
//! A value is either a double-quoted string with C-like escapes, possibly
//! continued by further quoted strings on the following lines, or bare text
//! up to the end of the line. Files saved by newer releases may end with an
//! embedded OPC package (`__MWOPC_PACKAGE_BEGIN__`), which is ignored.

use crate::tree::Node;
use crate::{ImportError, MAX_DEPTH, MAX_NODES};

/// Parse MDL text into its top-level sections (`Model`, `Library`,
/// `Stateflow`, `MatData`, ...).
pub fn parse(text: &str) -> Result<Vec<Node>, ImportError> {
    let mut stack: Vec<Node> = vec![Node::new("")];
    let mut lines = text.lines().enumerate().peekable();

    let mut budget = MAX_NODES;

    while let Some((lineno, raw)) = lines.next() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        budget = budget
            .checked_sub(1)
            .ok_or_else(|| ImportError::Mdl("too many elements".into()))?;
        if line.starts_with("__MWOPC_PACKAGE_BEGIN__") {
            break;
        }
        if line == "}" {
            if stack.len() < 2 {
                return Err(ImportError::Mdl(format!(
                    "line {}: unbalanced '}}'",
                    lineno + 1
                )));
            }
            let node = stack.pop().unwrap();
            stack.last_mut().unwrap().children.push(node);
            continue;
        }
        if let Some(tag) = line.strip_suffix('{') {
            if stack.len() > MAX_DEPTH {
                return Err(ImportError::Mdl(format!(
                    "line {}: nesting too deep",
                    lineno + 1
                )));
            }
            stack.push(Node::new(tag.trim()));
            continue;
        }

        let (key, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        let value = if rest.starts_with('"') {
            let mut value =
                unquote(rest).map_err(|e| ImportError::Mdl(format!("line {}: {e}", lineno + 1)))?;
            while let Some((_, next)) = lines.peek() {
                let next = next.trim();
                if !next.starts_with('"') {
                    break;
                }
                value.push_str(
                    &unquote(next)
                        .map_err(|e| ImportError::Mdl(format!("line {}: {e}", lineno + 2)))?,
                );
                lines.next();
            }
            value
        } else {
            rest.to_string()
        };
        stack
            .last_mut()
            .unwrap()
            .props
            .push((key.to_string(), value));
    }

    if stack.len() != 1 {
        return Err(ImportError::Mdl(format!(
            "unexpected end of file inside '{}'",
            stack.last().unwrap().tag
        )));
    }
    Ok(stack.pop().unwrap().children)
}

/// Decode one double-quoted MDL string literal occupying the whole of `s`.
fn unquote(s: &str) -> Result<String, String> {
    let body = s
        .strip_prefix('"')
        .and_then(|b| b.strip_suffix('"'))
        .ok_or_else(|| format!("malformed string literal: {s}"))?;
    let mut out = String::with_capacity(body.len());
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
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_sections_and_strings() {
        let text = r#"
Model {
  Name			  "demo"
  Version		  7.1
  System {
    Block {
      BlockType		      Gain
      Name		      "proportional\ngain"
      Position		      [95, 92, 145, 138]
      MaskDisplay	      "disp('a');"
      "disp('b')"
    }
  }
}
MatData {
  NumRecords 0
}
"#;
        let top = parse(text).unwrap();
        assert_eq!(top.len(), 2);
        let model = &top[0];
        assert_eq!(model.tag, "Model");
        assert_eq!(model.prop("Name"), Some("demo"));
        assert_eq!(model.prop("Version"), Some("7.1"));
        let block = &model.child("System").unwrap().children[0];
        assert_eq!(block.prop("BlockType"), Some("Gain"));
        assert_eq!(block.prop("Name"), Some("proportional\ngain"));
        assert_eq!(block.prop("Position"), Some("[95, 92, 145, 138]"));
        assert_eq!(block.prop("MaskDisplay"), Some("disp('a');disp('b')"));
    }

    #[test]
    fn escaped_quotes_survive() {
        let top = parse("Model {\n  Name \"a \\\"b\\\" c\"\n}\n").unwrap();
        assert_eq!(top[0].prop("Name"), Some("a \"b\" c"));
    }

    #[test]
    fn stops_at_embedded_package() {
        let top =
            parse("Model {\n  Name \"m\"\n}\n__MWOPC_PACKAGE_BEGIN__ R2013a\nZUVGb28=\n").unwrap();
        assert_eq!(top.len(), 1);
    }

    #[test]
    fn unbalanced_braces_error() {
        assert!(parse("Model {\n  Name \"m\"\n").is_err());
        assert!(parse("}\n").is_err());
    }
}
