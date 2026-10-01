//! Scope block configuration.
//!
//! Newer Simulink releases store a Scope's settings in
//! `ScopeSpecificationString`: a serialized MATLAB constructor call such as
//! `Simulink.scopes.TimeScopeBlockCfg('CurrentConfiguration', extmgr.ConfigurationSet(...), ...)`.
//! [`parse_spec`] reads that syntax into a generic [`SpecValue`] tree with a
//! small bounded parser, and [`ScopeConfig`] extracts the settings a viewer
//! needs. Older files keep plain `YMin`/`YMax`/`TimeRange`/`SaveName`
//! parameters instead, which [`ScopeConfig::from_block`] also reads. The raw
//! parameter is never rewritten.

use crate::Block;
use serde::{Deserialize, Serialize};

/// Largest specification string parsed.
const MAX_SPEC_BYTES: usize = 256 * 1024;
/// Deepest nesting of calls, matrices and cells.
const MAX_DEPTH: usize = 64;
/// Most values in one specification.
const MAX_NODES: usize = 100_000;

/// A value in a serialized specification.
#[derive(Debug, Clone, PartialEq)]
pub enum SpecValue {
    /// `name(args...)`, including `struct(...)`.
    Call {
        name: String,
        args: Vec<SpecValue>,
    },
    Str(String),
    Num(f64),
    Bool(bool),
    /// `[a b; c d]`, row by row.
    Matrix(Vec<Vec<f64>>),
    /// `{a, b}`.
    Cell(Vec<SpecValue>),
}

impl SpecValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            SpecValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A number, or a string holding one (limits are stored as strings).
    fn as_f64(&self) -> Option<f64> {
        match self {
            SpecValue::Num(n) => Some(*n),
            SpecValue::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            SpecValue::Bool(b) => Some(*b),
            SpecValue::Str(s) => match s.as_str() {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            },
            SpecValue::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }

    /// Value of `key` in a call's trailing `'Key', value` pairs.
    fn arg(&self, key: &str) -> Option<&SpecValue> {
        let SpecValue::Call { name, args } = self else {
            return None;
        };
        let prefix = match name.as_str() {
            "extmgr.Configuration" => 3,
            "struct" | "Simulink.scopes.TimeScopeBlockCfg" => 0,
            _ => return None,
        };
        args.get(prefix..)?
            .as_chunks::<2>()
            .0
            .iter()
            .find(|pair| pair[0].as_str() == Some(key))
            .map(|pair| &pair[1])
    }

    /// Every call named `name` at any depth, in order.
    fn calls<'a>(&'a self, name: &str, out: &mut Vec<&'a SpecValue>) {
        match self {
            SpecValue::Call { name: n, args } => {
                if n == name {
                    out.push(self);
                }
                args.iter().for_each(|a| a.calls(name, out));
            }
            SpecValue::Cell(items) => items.iter().for_each(|a| a.calls(name, out)),
            _ => {}
        }
    }

    /// All strings at any depth inside a (possibly nested) cell.
    fn strings(&self, out: &mut Vec<String>) {
        match self {
            SpecValue::Str(s) => out.push(s.clone()),
            SpecValue::Cell(items) => items.iter().for_each(|i| i.strings(out)),
            _ => {}
        }
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("{msg} at byte {}", self.pos))
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.src.get(self.pos).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<SpecValue, String> {
        if depth > MAX_DEPTH {
            return self.err("nesting too deep");
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return self.err("too many values");
        }
        match self.peek() {
            Some(b'\'') => self.string().map(SpecValue::Str),
            Some(b'[') => self.matrix(),
            Some(b'{') => self.cell(depth),
            Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') => {
                self.number().map(SpecValue::Num)
            }
            Some(c) if c.is_ascii_alphabetic() => self.call_or_word(depth),
            Some(_) => self.err("unexpected character"),
            None => self.err("unexpected end"),
        }
    }

    /// `'...'` with `''` as an escaped quote.
    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            match self.src.get(self.pos) {
                None => return self.err("unterminated string"),
                Some(b'\'') if self.src.get(self.pos + 1) == Some(&b'\'') => {
                    out.push(b'\'');
                    self.pos += 2;
                }
                Some(b'\'') => {
                    self.pos += 1;
                    return String::from_utf8(out).or_else(|_| self.err("invalid UTF-8"));
                }
                Some(&c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.src.len()
            && (self.src[self.pos].is_ascii_alphanumeric()
                || matches!(self.src[self.pos], b'.' | b'-' | b'+'))
        {
            // A sign only continues a number right after an exponent marker.
            if matches!(self.src[self.pos], b'-' | b'+')
                && self.pos > start
                && !matches!(self.src[self.pos - 1], b'e' | b'E')
            {
                break;
            }
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        match text {
            "Inf" | "inf" => Ok(f64::INFINITY),
            "-Inf" | "-inf" => Ok(f64::NEG_INFINITY),
            _ => text.parse().or_else(|_| self.err("invalid number")),
        }
    }

    fn matrix(&mut self) -> Result<SpecValue, String> {
        self.pos += 1;
        let mut rows = vec![Vec::new()];
        loop {
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    if rows.last().is_some_and(Vec::is_empty) {
                        rows.pop();
                    }
                    return Ok(SpecValue::Matrix(rows));
                }
                Some(b';') => {
                    self.pos += 1;
                    rows.push(Vec::new());
                }
                Some(b',') => self.pos += 1,
                Some(_) => {
                    self.nodes += 1;
                    if self.nodes > MAX_NODES {
                        return self.err("too many values");
                    }
                    let n = match self.peek() {
                        Some(c) if c.is_ascii_alphabetic() => match self.word().as_str() {
                            "true" => 1.0,
                            "false" => 0.0,
                            "Inf" | "inf" => f64::INFINITY,
                            "NaN" | "nan" => f64::NAN,
                            _ => return self.err("non-numeric matrix element"),
                        },
                        _ => self.number()?,
                    };
                    rows.last_mut().expect("rows is never empty here").push(n);
                }
                None => return self.err("unterminated matrix"),
            }
        }
    }

    fn cell(&mut self, depth: usize) -> Result<SpecValue, String> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(SpecValue::Cell(items));
                }
                Some(b',' | b';') => self.pos += 1,
                Some(_) => items.push(self.value(depth + 1)?),
                None => return self.err("unterminated cell"),
            }
        }
    }

    fn word(&mut self) -> String {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.src.len()
            && (self.src[self.pos].is_ascii_alphanumeric()
                || matches!(self.src[self.pos], b'_' | b'.'))
        {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()
    }

    fn call_or_word(&mut self, depth: usize) -> Result<SpecValue, String> {
        let name = self.word();
        match name.as_str() {
            "true" => return Ok(SpecValue::Bool(true)),
            "false" => return Ok(SpecValue::Bool(false)),
            "Inf" | "inf" => return Ok(SpecValue::Num(f64::INFINITY)),
            "NaN" | "nan" => return Ok(SpecValue::Num(f64::NAN)),
            _ => {}
        }
        if !self.eat(b'(') {
            return self.err(&format!("expected '(' after {name}"));
        }
        let mut args = Vec::new();
        loop {
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(SpecValue::Call { name, args });
                }
                Some(b',') => self.pos += 1,
                Some(_) => args.push(self.value(depth + 1)?),
                None => return self.err("unterminated call"),
            }
        }
    }
}

/// Parse a serialized specification string.
pub fn parse_spec(source: &str) -> Result<SpecValue, String> {
    if source.len() > MAX_SPEC_BYTES {
        return Err("specification too large".into());
    }
    let mut p = Parser {
        src: source.as_bytes(),
        pos: 0,
        nodes: 0,
    };
    let v = p.value(0)?;
    if p.peek().is_some() {
        return p.err("trailing text");
    }
    Ok(v)
}

/// One axes of a scope window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeDisplay {
    /// May contain the placeholder `%<SignalLabel>`.
    pub title: Option<String>,
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
    pub y_label: Option<String>,
    pub legend: Option<bool>,
    pub x_grid: Option<bool>,
    pub y_grid: Option<bool>,
    /// Names of the plotted lines.
    pub line_names: Vec<String>,
}

/// Settings of a Scope block, as far as the file records them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeConfig {
    /// Configuration class, e.g. `Simulink.scopes.TimeScopeBlockCfg`, or
    /// `None` for the legacy parameter format.
    pub kind: Option<String>,
    /// Whether logging is explicitly enabled; absent flags remain unknown.
    pub logging_enabled: Option<bool>,
    /// Configured workspace variable, even when logging is disabled or unknown.
    pub logging_variable: Option<String>,
    /// Visible time span in seconds (`auto` is `None`).
    pub time_span: Option<f64>,
    pub displays: Vec<ScopeDisplay>,
    /// Scope window `[left, top, width, height]` in screen pixels.
    pub window: Option<[f64; 4]>,
    pub open_at_start: Option<bool>,
    pub version: Option<String>,
}

fn display(d: &SpecValue) -> ScopeDisplay {
    let str_arg = |k: &str| {
        d.arg(k)
            .and_then(SpecValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut line_names = Vec::new();
    if let Some(n) = d.arg("LineNames") {
        n.strings(&mut line_names);
    }
    ScopeDisplay {
        title: str_arg("Title"),
        y_min: d.arg("MinYLimReal").and_then(SpecValue::as_f64),
        y_max: d.arg("MaxYLimReal").and_then(SpecValue::as_f64),
        y_label: str_arg("YLabelReal"),
        legend: d.arg("LegendVisibility").and_then(SpecValue::as_bool),
        x_grid: d.arg("XGrid").and_then(SpecValue::as_bool),
        y_grid: d.arg("YGrid").and_then(SpecValue::as_bool),
        line_names,
    }
}

impl ScopeConfig {
    /// Read a parsed `ScopeSpecificationString`.
    pub fn from_spec(spec: &SpecValue) -> ScopeConfig {
        let SpecValue::Call { name, .. } = spec else {
            return ScopeConfig::default();
        };
        let mut configs = Vec::new();
        spec.calls("extmgr.Configuration", &mut configs);
        // extmgr.Configuration(type, name, enabled, 'Key', value, ...)
        let section = |kind: &str, label: &str| {
            configs.iter().copied().find(|c| match c {
                SpecValue::Call { args, .. } => {
                    args.first().and_then(SpecValue::as_str) == Some(kind)
                        && args.get(1).and_then(SpecValue::as_str) == Some(label)
                }
                _ => false,
            })
        };
        let visuals = section("Visuals", "Time Domain");
        let mut displays = Vec::new();
        if let Some(SpecValue::Cell(items)) = visuals.and_then(|v| v.arg("SerializedDisplays")) {
            displays = items.iter().map(display).collect();
        }
        let time_span = visuals
            .and_then(|v| v.arg("TimeRangeFrames").or_else(|| v.arg("TimeSpan")))
            .and_then(SpecValue::as_f64);
        let window = match spec.arg("Position") {
            Some(SpecValue::Matrix(rows)) if rows.len() == 1 && rows[0].len() == 4 => {
                Some([rows[0][0], rows[0][1], rows[0][2], rows[0][3]])
            }
            _ => None,
        };
        ScopeConfig {
            kind: Some(name.clone()),
            logging_enabled: section("Sources", "WiredSimulink")
                .and_then(|s| s.arg("DataLogging"))
                .and_then(SpecValue::as_bool),
            logging_variable: section("Sources", "WiredSimulink")
                .and_then(|s| s.arg("DataLoggingVariableName"))
                .and_then(SpecValue::as_str)
                .map(str::to_string),
            time_span,
            displays,
            window,
            open_at_start: spec.arg("VisibleAtModelOpen").and_then(SpecValue::as_bool),
            version: spec
                .arg("Version")
                .and_then(SpecValue::as_str)
                .map(str::to_string),
        }
    }

    /// Settings of a Scope block: `None` for other blocks, an error when the
    /// specification string cannot be parsed.
    pub fn from_block(block: &Block) -> Option<Result<ScopeConfig, String>> {
        if block.block_type != "Scope" {
            return None;
        }
        if let Some(spec) = block.param("ScopeSpecificationString") {
            return Some(parse_spec(spec).and_then(|s| {
                match &s {
                    SpecValue::Call { name, .. } if name == "Simulink.scopes.TimeScopeBlockCfg" => {
                        Ok(ScopeConfig::from_spec(&s))
                    }
                    _ => Err("unsupported scope specification root; expected Simulink.scopes.TimeScopeBlockCfg".into()),
                }
            }));
        }
        // Legacy parameters.
        let num = |k: &str| block.param(k).and_then(|v| v.trim().parse().ok());
        let displays = match (num("YMin"), num("YMax")) {
            (None, None) => Vec::new(),
            (y_min, y_max) => vec![ScopeDisplay {
                y_min,
                y_max,
                ..Default::default()
            }],
        };
        Some(Ok(ScopeConfig {
            logging_enabled: block.param("SaveToWorkspace").and_then(|v| match v {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            }),
            logging_variable: block.param("SaveName").map(str::to_string),
            time_span: num("TimeRange"),
            displays,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Simulink.scopes.TimeScopeBlockCfg('CurrentConfiguration', extmgr.ConfigurationSet(\
        extmgr.Configuration('Core','General UI',true),\
        extmgr.Configuration('Sources','WiredSimulink',true,'DataLoggingVariableName','ScopeData1'),\
        extmgr.Configuration('Visuals','Time Domain',true,'SerializedDisplays',{struct('MinYLimReal','-0.000022','MaxYLimReal','0.000021','YLabelReal','','LegendVisibility','off','XGrid',true,'YGrid',true,'AxesColor',[0 0 0],'ColorOrder',[1 1 0.06;0.07 0.62 1],'Title','%<SignalLabel>','LinePropertiesCache',{{}},'NumLines',1,'LineNames',{{'Integrator1'}},'ShowContent',true,'Placement',1)},'DisplayPropertyDefaults',struct('MinYLimReal','-1')),\
        extmgr.Configuration('Tools','Measurements',true,'Version','2020b')),\
        'Version','2020b','Position',[-1447.8 302.6 560.8 420],'VisibleAtModelOpen','on')";

    #[test]
    fn time_scope_spec_is_structured() {
        let cfg = ScopeConfig::from_spec(&parse_spec(SPEC).unwrap());
        assert_eq!(
            cfg.kind.as_deref(),
            Some("Simulink.scopes.TimeScopeBlockCfg")
        );
        assert_eq!(cfg.logging_variable.as_deref(), Some("ScopeData1"));
        assert_eq!(cfg.version.as_deref(), Some("2020b"));
        assert_eq!(cfg.window, Some([-1447.8, 302.6, 560.8, 420.0]));
        assert_eq!(cfg.open_at_start, Some(true));
        assert_eq!(cfg.displays.len(), 1);
        let d = &cfg.displays[0];
        assert_eq!(d.y_min, Some(-0.000022));
        assert_eq!(d.y_max, Some(0.000021));
        assert_eq!(d.y_label, None);
        assert_eq!(d.legend, Some(false));
        assert_eq!((d.x_grid, d.y_grid), (Some(true), Some(true)));
        assert_eq!(d.title.as_deref(), Some("%<SignalLabel>"));
        assert_eq!(d.line_names, vec!["Integrator1".to_string()]);
    }

    fn scope_block(spec: &str) -> Block {
        Block {
            id: "scope".into(),
            block_type: "Scope".into(),
            name: "Scope".into(),
            position: Default::default(),
            orientation: crate::Orientation::Right,
            mirrored: false,
            ports: Default::default(),
            parameters: [("ScopeSpecificationString".into(), spec.into())]
                .into_iter()
                .collect(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: Default::default(),
            interface: None,
        }
    }

    #[test]
    fn property_values_are_not_keys() {
        let d = display(
            &parse_spec("struct('Title','MinYLimReal','YLabelReal','3','MinYLimReal','-2')")
                .unwrap(),
        );
        assert_eq!(d.title.as_deref(), Some("MinYLimReal"));
        assert_eq!(d.y_min, Some(-2.0));
        let spec = parse_spec("extmgr.Configuration('Sources','DataLogging',true,'Title','DataLogging','DataLogging',false)").unwrap();
        assert_eq!(
            spec.arg("DataLogging").and_then(SpecValue::as_bool),
            Some(false)
        );
        let spec =
            parse_spec("Simulink.scopes.TimeScopeBlockCfg('Title','Version','Version','2020b')")
                .unwrap();
        assert_eq!(
            spec.arg("Version").and_then(SpecValue::as_str),
            Some("2020b")
        );
    }

    #[test]
    fn unsupported_root_is_reported_but_generic_parser_stays_generic() {
        for text in ["true", "42", "'hello'", "struct()", "arbitrary()"] {
            assert!(parse_spec(text).is_ok());
            assert!(ScopeConfig::from_block(&scope_block(text))
                .unwrap()
                .is_err());
        }
        assert!(ScopeConfig::from_block(&scope_block(SPEC)).unwrap().is_ok());
    }

    #[test]
    fn logging_is_not_implied_by_variable_name() {
        let cfg = ScopeConfig::from_block(&scope_block(SPEC))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.logging_enabled, None);
        assert_eq!(cfg.logging_variable.as_deref(), Some("ScopeData1"));
        for (flag, expected) in [
            ("false", false),
            ("true", true),
            ("'off'", false),
            ("'on'", true),
        ] {
            let text = SPEC.replace(
                "'DataLoggingVariableName'",
                &format!("'DataLogging',{flag},'DataLoggingVariableName'"),
            );
            let cfg = ScopeConfig::from_block(&scope_block(&text))
                .unwrap()
                .unwrap();
            assert_eq!(cfg.logging_enabled, Some(expected));
            assert_eq!(cfg.logging_variable.as_deref(), Some("ScopeData1"));
        }
        let mut block = scope_block(SPEC);
        block.parameters.clear();
        block
            .parameters
            .insert("SaveToWorkspace".into(), "off".into());
        block
            .parameters
            .insert("SaveName".into(), "ScopeData".into());
        let cfg = ScopeConfig::from_block(&block).unwrap().unwrap();
        assert_eq!(cfg.logging_enabled, Some(false));
        assert_eq!(cfg.logging_variable.as_deref(), Some("ScopeData"));
    }

    #[test]
    fn syntax_details() {
        assert_eq!(
            parse_spec("'it''s'").unwrap(),
            SpecValue::Str("it's".into())
        );
        assert_eq!(
            parse_spec("[1 -2.5e-3; 3, Inf]").unwrap(),
            SpecValue::Matrix(vec![vec![1.0, -2.5e-3], vec![3.0, f64::INFINITY]])
        );
        assert_eq!(parse_spec("[]").unwrap(), SpecValue::Matrix(vec![]));
        assert!(parse_spec("f(1").is_err());
        assert!(parse_spec("'open").is_err());
        assert!(parse_spec("f(1) extra").is_err());
    }

    #[test]
    fn hostile_input_is_bounded() {
        let deep = format!("{}1{}", "{".repeat(10_000), "}".repeat(10_000));
        assert!(parse_spec(&deep).is_err());
        // Under the byte cap but over the value cap.
        let wide = format!("[{}]", "1 ".repeat(MAX_NODES + 1));
        assert!(parse_spec(&wide).is_err());
        assert!(parse_spec(&"a".repeat(MAX_SPEC_BYTES + 1)).is_err());
    }
}
