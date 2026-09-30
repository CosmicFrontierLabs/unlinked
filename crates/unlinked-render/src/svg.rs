//! Minimal SVG string builder with attribute/text escaping.

use std::fmt::Write;

pub struct Svg {
    out: String,
    elements: usize,
}

/// Format a coordinate compactly: integers without a fraction, others with
/// at most two decimals.
pub fn num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // Characters XML 1.0 forbids.
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

impl Svg {
    pub fn new() -> Self {
        Svg {
            out: String::new(),
            elements: 0,
        }
    }

    pub fn finish(self) -> String {
        self.out
    }

    /// Number of elements written so far.
    pub fn elements(&self) -> usize {
        self.elements
    }

    pub fn raw(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// Open an element. `attrs` values are escaped.
    pub fn open(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.elements += 1;
        self.out.push('<');
        self.out.push_str(tag);
        self.attrs(attrs);
        self.out.push('>');
    }

    pub fn close(&mut self, tag: &str) {
        let _ = write!(self.out, "</{tag}>");
    }

    /// A self-closing element.
    pub fn leaf(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.elements += 1;
        self.out.push('<');
        self.out.push_str(tag);
        self.attrs(attrs);
        self.out.push_str("/>");
    }

    fn attrs(&mut self, attrs: &[(&str, String)]) {
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", escape(v));
        }
    }

    /// Text element; `lines` are stacked with `line_height` spacing, the
    /// block of lines vertically centred on `y` when `v_center` is set,
    /// otherwise the first baseline sits at `y`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        x: f64,
        y: f64,
        lines: &[&str],
        size: f64,
        anchor: &str,
        v_center: bool,
        extra: &[(&str, String)],
    ) {
        if lines.is_empty() {
            return;
        }
        let lh = size * 1.15;
        let first = if v_center {
            y - lh * (lines.len() as f64 - 1.0) / 2.0 + size * 0.35
        } else {
            y
        };
        let mut attrs = vec![
            ("x", num(x)),
            ("y", num(first)),
            ("font-size", num(size)),
            ("text-anchor", anchor.to_string()),
        ];
        attrs.extend(extra.iter().cloned());
        self.open("text", &attrs);
        for (i, line) in lines.iter().enumerate() {
            if i == 0 {
                self.out.push_str(&escape(line));
            } else {
                let _ = write!(
                    self.out,
                    "<tspan x=\"{}\" dy=\"{}\">{}</tspan>",
                    num(x),
                    num(lh),
                    escape(line)
                );
            }
        }
        self.close("text");
    }
}

pub fn points_attr(pts: &[(f64, f64)]) -> String {
    pts.iter()
        .map(|(x, y)| format!("{},{}", num(*x), num(*y)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_compact() {
        assert_eq!(num(3.0), "3");
        assert_eq!(num(-2.5), "-2.5");
        assert_eq!(num(1.0 / 3.0), "0.33");
    }

    #[test]
    fn escapes_markup() {
        assert_eq!(escape("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
        assert_eq!(escape("x\u{1}y\u{FFFE}"), "xy");
    }
}
