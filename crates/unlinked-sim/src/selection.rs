//! Shared parsing for the Signal Routing selection blocks (Selector, Multiport
//! Switch, Manual Switch and the Switch threshold criteria). Dimensional
//! resolution lives in `vector.rs`; the per-step Multiport Switch index is the
//! only runtime state these blocks need (`Kind::MultiPortSwitch`).
//!
//! References:
//! - <https://www.mathworks.com/help/simulink/slref/selector.html>
//! - <https://www.mathworks.com/help/simulink/slref/multiportswitch.html>
//! - <https://www.mathworks.com/help/simulink/slref/indexvector.html>
//! - <https://www.mathworks.com/help/simulink/slref/manualswitch.html>
//! - <https://www.mathworks.com/help/simulink/slref/environmentcontroller.html>
//! - <https://www.mathworks.com/help/simulink/slref/switch.html>

/// Split a MATLAB cell literal such as `{'Index vector (dialog)','Select all'}`
/// or `{[3,5],0,18}` into its top-level items with surrounding quotes removed.
/// A value without braces (the older comma-separated `IndexOptions` and
/// `Indices` spellings, `[1 3],1`) splits at its top-level commas. Nested
/// brackets and quoted commas stay inside their item. Nothing is evaluated.
pub(crate) fn cell_items(source: &str) -> Result<Vec<String>, String> {
    let trimmed = source.trim();
    if trimmed.len() > 64 * 1024 {
        return Err("cell array literal exceeds 64 KiB".into());
    }
    let inner = match trimmed.strip_prefix('{') {
        Some(rest) => rest
            .strip_suffix('}')
            .ok_or("unterminated cell array literal")?,
        None => trimmed,
    };
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                // Doubled quotes escape themselves inside MATLAB strings.
                if chars.peek() == Some(&q) {
                    current.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                current.push(c);
            }
            '[' | '(' | '{' => {
                depth += 1;
                current.push(c);
            }
            ']' | ')' | '}' => {
                depth = depth.checked_sub(1).ok_or("unbalanced brackets")?;
                current.push(c);
            }
            ',' | ';' if depth == 0 => {
                items.push(unquote(&current)?);
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if quote.is_some() || depth != 0 {
        return Err("unbalanced quotes or brackets in cell array literal".into());
    }
    if !current.trim().is_empty() || !items.is_empty() {
        items.push(unquote(&current)?);
    }
    Ok(items)
}

fn unquote(item: &str) -> Result<String, String> {
    let item = item.trim();
    for q in ['\'', '"'] {
        if let Some(inner) = item.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return Ok(inner.replace(&format!("{q}{q}"), &q.to_string()));
        }
    }
    if item.starts_with(['\'', '"']) {
        return Err(format!("unterminated string {item}"));
    }
    Ok(item.to_string())
}

/// One Selector dimension after dialog values resolve, with zero-based
/// indices into the input. Port-driven index options never reach this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DimensionSelect {
    All,
    Indices(Vec<usize>),
}

/// Multiport Switch configuration in its canonical lowered form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MultiPortConfig {
    /// Control values (after truncation toward zero) that select each data port.
    pub indices: Vec<Vec<i64>>,
    /// An extra trailing data port carries the default case.
    pub additional_default_port: bool,
    /// Whether an unmatched control value stops the run instead of using the
    /// default data port.
    pub error_on_default: bool,
}

impl MultiPortConfig {
    pub(crate) fn data_ports(&self) -> usize {
        self.indices.len() + usize::from(self.additional_default_port)
    }
    /// Zero-based data port used when no index matches, unless that is an error.
    pub(crate) fn default_port(&self) -> Option<usize> {
        if self.error_on_default {
            None
        } else if self.additional_default_port {
            Some(self.indices.len())
        } else {
            self.indices.len().checked_sub(1)
        }
    }
    /// Canonical `DataPortIndices` literal, `{[3 5],[0],[18]}`.
    pub(crate) fn indices_literal(&self) -> String {
        let items: Vec<String> = self
            .indices
            .iter()
            .map(|port| {
                format!(
                    "[{}]",
                    port.iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
            .collect();
        format!("{{{}}}", items.join(","))
    }
    /// Parse the canonical literal written by `indices_literal`.
    pub(crate) fn parse_indices_literal(source: &str) -> Result<Vec<Vec<i64>>, String> {
        cell_items(source)?
            .iter()
            .map(|item| {
                let inner = item
                    .trim()
                    .strip_prefix('[')
                    .and_then(|s| s.strip_suffix(']'))
                    .ok_or("DataPortIndices entries must be bracketed integer lists")?;
                inner
                    .split_whitespace()
                    .map(|n| {
                        n.parse::<i64>()
                            .map_err(|_| format!("invalid data port index {n}"))
                    })
                    .collect()
            })
            .collect()
    }
}

/// How a Switch block decides to pass its first input, evaluated on doubles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwitchCriteria {
    Nonzero,
    /// `u2 >= Threshold`
    AtLeast,
    /// `u2 > Threshold`
    Above,
}

impl SwitchCriteria {
    /// Imported blocks without `Criteria` use `u2 >= Threshold`, the built-in
    /// default Simulink writes to `BlockParameterDefaults` (seen in the
    /// corpus, e.g. spaaro `sig_kadet.slx`). The reference page's
    /// `Default: 'u2 > Threshold'` describes the library block, whose copies
    /// store `Criteria` explicitly; the two route the boundary value
    /// differently, so the file-level default is the one that applies here.
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or("u2 >= Threshold") {
            "u2 ~= 0" => Ok(Self::Nonzero),
            "u2 >= Threshold" => Ok(Self::AtLeast),
            "u2 > Threshold" => Ok(Self::Above),
            other => Err(format!("unsupported Switch Criteria={other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_literals_split_at_top_level_only() {
        assert_eq!(
            cell_items("{'Index vector (dialog)','Select all'}").unwrap(),
            vec!["Index vector (dialog)", "Select all"]
        );
        assert_eq!(
            cell_items("{[3,5],0,18}").unwrap(),
            vec!["[3,5]", "0", "18"]
        );
        assert_eq!(cell_items("{\"[1 3]\"}").unwrap(), vec!["[1 3]"]);
        assert_eq!(cell_items("[1 3]").unwrap(), vec!["[1 3]"]);
        assert_eq!(cell_items("[1,3],1").unwrap(), vec!["[1,3]", "1"]);
        assert_eq!(
            cell_items("Index vector (dialog),Select all").unwrap(),
            vec!["Index vector (dialog)", "Select all"]
        );
        assert_eq!(cell_items("{}").unwrap(), Vec::<String>::new());
        assert_eq!(cell_items("").unwrap(), Vec::<String>::new());
        assert_eq!(cell_items("{'it''s'}").unwrap(), vec!["it's"]);
        assert!(cell_items("{'a'").is_err());
        assert!(cell_items("{[1,2}").is_err());
        assert!(cell_items("{'a}").is_err());
    }

    #[test]
    fn canonical_indices_round_trip() {
        let config = MultiPortConfig {
            indices: vec![vec![3, 5], vec![0], vec![-18]],
            additional_default_port: true,
            error_on_default: false,
        };
        let literal = config.indices_literal();
        assert_eq!(literal, "{[3 5],[0],[-18]}");
        assert_eq!(
            MultiPortConfig::parse_indices_literal(&literal).unwrap(),
            config.indices
        );
        assert_eq!(config.data_ports(), 4);
        assert_eq!(config.default_port(), Some(3));
        assert!(MultiPortConfig::parse_indices_literal("{1,2}").is_err());
    }

    #[test]
    fn switch_criteria_default_to_threshold_comparison() {
        assert_eq!(
            SwitchCriteria::parse(None).unwrap(),
            SwitchCriteria::AtLeast
        );
        assert_eq!(
            SwitchCriteria::parse(Some("u2 ~= 0")).unwrap(),
            SwitchCriteria::Nonzero
        );
        assert!(SwitchCriteria::parse(Some("u2 < Threshold")).is_err());
    }
}
