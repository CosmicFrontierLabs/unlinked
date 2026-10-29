//! Block appearance: outline shape and icon content per block type.

use crate::svg::{num, points_attr, Svg};
use unlinked_model::{Block, Orientation, Rect, System};

/// What goes inside a block's outline.
pub enum Icon {
    None,
    /// Centred lines of text.
    Text(Vec<String>),
    /// Numerator over denominator with a fraction bar.
    Fraction(String, String),
    /// Polylines in the unit square (x right, y down), drawn with padding.
    Strokes(Vec<Vec<(f64, f64)>>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    Rect,
    Rounded,
    Circle,
    Triangle,
    Bar,
}

pub struct Appearance {
    pub shape: Shape,
    pub icon: Icon,
}

pub fn appearance(block: &Block) -> Appearance {
    let p = |k: &str| block.param(k).unwrap_or_default().trim().to_string();
    let text = |s: &str| Icon::Text(s.lines().map(str::to_string).collect());
    let shape = Shape::Rect;
    let icon = match block.block_type.as_str() {
        "Gain" => {
            // A triangle leaves about half its box for text.
            let g = shown_value(block, non_empty(p("Gain"), "1"), 0.5, "-K-");
            return Appearance {
                shape: Shape::Triangle,
                icon: text(&g),
            };
        }
        "Sum" => {
            let round = block.param("IconShape") == Some("round");
            let signs: String = p("Inputs")
                .chars()
                .filter(|c| matches!(c, '+' | '-'))
                .collect();
            return Appearance {
                shape: if round { Shape::Circle } else { Shape::Rect },
                icon: if round {
                    Icon::Text(vec![sum_round_label(&signs)])
                } else {
                    text("Σ")
                },
            };
        }
        "Inport" | "Outport" | "InportShadow" => {
            let port = p("Port");
            return Appearance {
                shape: Shape::Rounded,
                icon: text(if port.is_empty() { "1" } else { &port }),
            };
        }
        "Mux" | "Demux" | "BusCreator" | "BusSelector" if p("DisplayOption") != "signals" => {
            return Appearance {
                shape: Shape::Bar,
                icon: Icon::None,
            };
        }
        "Constant" => text(&shown_value(block, non_empty(p("Value"), "1"), 0.85, "-C-")),
        "Product" => {
            let inputs = p("Inputs");
            if inputs.contains('/') {
                text("÷")
            } else {
                text("×")
            }
        }
        "Integrator" => Icon::Fraction("1".into(), "s".into()),
        "DiscreteIntegrator" => Icon::Fraction("K Ts".into(), "z-1".into()),
        "Derivative" => Icon::Fraction("du".into(), "dt".into()),
        "UnitDelay" => Icon::Fraction("1".into(), "z".into()),
        "Delay" => Icon::Fraction("1".into(), "z^-d".into()),
        "Memory" => text("Memory"),
        "ZeroOrderHold" => Icon::Strokes(vec![vec![
            (0.1, 0.8),
            (0.3, 0.8),
            (0.3, 0.55),
            (0.5, 0.55),
            (0.5, 0.35),
            (0.7, 0.35),
            (0.7, 0.2),
            (0.9, 0.2),
        ]]),
        "TransferFcn" => Icon::Fraction(poly(&p("Numerator"), "s"), poly(&p("Denominator"), "s")),
        "DiscreteTransferFcn" => {
            Icon::Fraction(poly(&p("Numerator"), "z"), poly(&p("Denominator"), "z"))
        }
        "ZeroPole" => text("(s-z)\n(s-p)"),
        "StateSpace" => text("x' = Ax+Bu\ny = Cx+Du"),
        "DiscreteStateSpace" => text("x(n+1)=Ax+Bu\ny(n)=Cx+Du"),
        "Step" => Icon::Strokes(vec![vec![(0.1, 0.8), (0.5, 0.8), (0.5, 0.2), (0.9, 0.2)]]),
        "Sin" => Icon::Strokes(vec![sine()]),
        "SignalGenerator" => Icon::Strokes(vec![sine(), vec![(0.1, 0.8), (0.1, 0.2)]]),
        "Clock" => text("⏲"),
        "DigitalClock" => text("12:34"),
        "Ramp" => Icon::Strokes(vec![vec![(0.1, 0.85), (0.9, 0.15)]]),
        "RandomNumber" | "UniformRandomNumber" | "BandLimitedWhiteNoise" => {
            Icon::Strokes(vec![noise()])
        }
        "DiscretePulseGenerator" => Icon::Strokes(vec![vec![
            (0.1, 0.8),
            (0.25, 0.8),
            (0.25, 0.2),
            (0.45, 0.2),
            (0.45, 0.8),
            (0.6, 0.8),
            (0.6, 0.2),
            (0.8, 0.2),
            (0.8, 0.8),
            (0.9, 0.8),
        ]]),
        "Scope" | "FloatingScope" => Icon::Strokes(vec![
            vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.7), (0.2, 0.7), (0.2, 0.2)],
            vec![
                (0.25, 0.55),
                (0.4, 0.35),
                (0.55, 0.55),
                (0.7, 0.35),
                (0.75, 0.4),
            ],
        ]),
        "Display" => text("0"),
        "ToWorkspace" => text(&non_empty(p("VariableName"), "simout")),
        "FromWorkspace" => text(&non_empty(p("VariableName"), "simin")),
        "ToFile" => text(&non_empty(p("Filename"), "untitled.mat")),
        "FromFile" => text(&non_empty(p("FileName"), "untitled.mat")),
        "Terminator" => Icon::Strokes(vec![
            vec![(0.3, 0.2), (0.3, 0.8)],
            vec![(0.3, 0.5), (0.7, 0.5)],
        ]),
        "Ground" => Icon::Strokes(vec![
            vec![(0.2, 0.5), (0.6, 0.5)],
            vec![(0.6, 0.2), (0.6, 0.8)],
            vec![(0.7, 0.3), (0.7, 0.7)],
            vec![(0.8, 0.4), (0.8, 0.6)],
        ]),
        "Saturate" => Icon::Strokes(vec![vec![
            (0.1, 0.75),
            (0.35, 0.75),
            (0.65, 0.25),
            (0.9, 0.25),
        ]]),
        "DeadZone" => Icon::Strokes(vec![vec![(0.1, 0.8), (0.4, 0.5), (0.6, 0.5), (0.9, 0.2)]]),
        "Backlash" => Icon::Strokes(vec![vec![
            (0.2, 0.8),
            (0.6, 0.2),
            (0.8, 0.2),
            (0.4, 0.8),
            (0.2, 0.8),
        ]]),
        "Relay" => Icon::Strokes(vec![
            vec![(0.1, 0.75), (0.6, 0.75), (0.6, 0.25), (0.9, 0.25)],
            vec![(0.4, 0.75), (0.4, 0.25), (0.6, 0.25)],
        ]),
        "Quantizer" => Icon::Strokes(vec![vec![
            (0.1, 0.9),
            (0.3, 0.9),
            (0.3, 0.7),
            (0.5, 0.7),
            (0.5, 0.5),
            (0.7, 0.5),
            (0.7, 0.3),
            (0.9, 0.3),
        ]]),
        "RateLimiter" => Icon::Strokes(vec![vec![(0.1, 0.8), (0.4, 0.8), (0.6, 0.2), (0.9, 0.2)]]),
        "Switch" => Icon::Strokes(vec![
            vec![(0.1, 0.2), (0.35, 0.2), (0.75, 0.5), (0.9, 0.5)],
            vec![(0.1, 0.8), (0.35, 0.8)],
        ]),
        "MultiPortSwitch" => text("*"),
        "ManualSwitch" => {
            // The arm shows the connected input: position 1 the first (upper),
            // 0 the second. Files store `CurrentSetting`; the programmatic `sw`
            // wins when present. https://www.mathworks.com/help/simulink/slref/manualswitch.html
            let sw = p("sw");
            let position = if sw.is_empty() {
                p("CurrentSetting")
            } else {
                sw
            };
            let from = if position == "0" { 0.7 } else { 0.3 };
            Icon::Strokes(vec![
                vec![(0.1, 0.3), (0.3, 0.3)],
                vec![(0.1, 0.7), (0.3, 0.7)],
                vec![(0.3, from), (0.7, 0.5), (0.9, 0.5)],
            ])
        }
        "RelationalOperator" => text(&relop(&non_empty(p("Operator"), ">="))),
        "Logic" => text(&non_empty(p("Operator"), "AND")),
        "Abs" => text("|u|"),
        "Signum" => text("sign"),
        "Sqrt" => text(&sqrt_label(&non_empty(p("Operator"), "sqrt"))),
        "Rounding" => text(&non_empty(p("Operator"), "floor")),
        "UnaryMinus" => text("-u"),
        "Polyval" => text("P(u)"),
        "DotProduct" => text("u₁·u₂"),
        "Math" => text(&math_label(&non_empty(p("Operator"), "exp"))),
        "Trigonometry" => text(&non_empty(p("Operator"), "sin")),
        "MinMax" => text(&non_empty(p("Function"), "min")),
        "Fcn" => text("f(u)"),
        "MATLABFcn" | "Interpreted MATLAB Function" => text("MATLAB\nFunction"),
        "Lookup" | "Lookup_n-D" | "Lookup2D" => Icon::Strokes(vec![vec![
            (0.15, 0.8),
            (0.4, 0.55),
            (0.6, 0.5),
            (0.85, 0.2),
        ]]),
        "Selector" => text("Selector"),
        "Assignment" => text("Assignment"),
        "Reshape" => text("U( : )"),
        "Concatenate" => text("Concatenate"),
        "DataTypeConversion" => text(&conversion_label(&p("OutDataTypeStr"))),
        "SignalConversion" => text("Signal\nConversion"),
        "Goto" => text(&format!("[{}]", non_empty(p("GotoTag"), "A"))),
        "From" => text(&format!("[{}]", non_empty(p("GotoTag"), "A"))),
        "GotoTagVisibility" => text(&format!("{{{}}}", non_empty(p("GotoTag"), "A"))),
        "DataStoreMemory" => text(&non_empty(p("DataStoreName"), "A")),
        "DataStoreRead" | "DataStoreWrite" => text(&non_empty(p("DataStoreName"), "A")),
        "Merge" => text("Merge"),
        "EnablePort" => Icon::Strokes(vec![vec![
            (0.2, 0.7),
            (0.4, 0.7),
            (0.4, 0.3),
            (0.6, 0.3),
            (0.6, 0.7),
            (0.8, 0.7),
        ]]),
        "TriggerPort" => Icon::Strokes(vec![
            vec![(0.2, 0.7), (0.5, 0.7), (0.5, 0.3), (0.8, 0.3)],
            vec![(0.4, 0.45), (0.5, 0.3), (0.6, 0.45)],
        ]),
        "ActionPort" => text("Action"),
        "If" => text("if(u1)\nelse"),
        "SwitchCase" => text("case\ndefault"),
        "Stop" => text("STOP"),
        "S-Function" => text(&non_empty(p("FunctionName"), "S-Function")),
        "ModelReference" => text(&non_empty(p("ModelName"), "Model")),
        "SubSystem" => subsystem_icon(block),
        "Reference" => reference_icon(block),
        _ => text(block.display_type()),
    };
    Appearance { shape, icon }
}

fn subsystem_icon(block: &Block) -> Icon {
    if block.param("SFBlockType").is_some_and(|t| t != "NONE") {
        return Icon::Strokes(vec![
            vec![
                (0.15, 0.25),
                (0.45, 0.25),
                (0.45, 0.5),
                (0.15, 0.5),
                (0.15, 0.25),
            ],
            vec![
                (0.55, 0.5),
                (0.85, 0.5),
                (0.85, 0.75),
                (0.55, 0.75),
                (0.55, 0.5),
            ],
            vec![(0.45, 0.37), (0.7, 0.37), (0.7, 0.5)],
        ]);
    }
    if let Some(display) = block.mask.as_ref().and_then(|m| m.display.as_deref()) {
        let lines = mask_display_text(display);
        if !lines.is_empty() {
            return Icon::Text(lines);
        }
    }
    Icon::None
}

fn reference_icon(block: &Block) -> Icon {
    // Unmasked library blocks store an empty SourceType; their library name
    // is the better label then.
    let source_type = block
        .param("SourceType")
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| block.display_type().replace('\n', " "));
    let p = |k: &str| block.param(k).unwrap_or_default().trim().to_string();
    match source_type.as_str() {
        "Index Vector" => Icon::Text(vec!["Index".into(), "Vector".into()]),
        "Environment Controller" => Icon::Text(vec!["Sim".into(), "Coder".into()]),
        "Compare To Zero" => Icon::Text(vec![format!("{} 0", relop(&non_empty(p("relop"), "<=")))]),
        "Compare To Constant" => Icon::Text(vec![format!(
            "{} {}",
            relop(&non_empty(p("relop"), "<=")),
            non_empty(p("const"), "3")
        )]),
        "Ramp" => Icon::Strokes(vec![vec![(0.1, 0.85), (0.9, 0.15)]]),
        "Chirp Signal" | "chirp" => Icon::Strokes(vec![chirp()]),
        "Repeating Sequence" | "Repeating table" => Icon::Strokes(vec![vec![
            (0.1, 0.8),
            (0.3, 0.2),
            (0.5, 0.8),
            (0.7, 0.2),
            (0.9, 0.8),
        ]]),
        "DocBlock" => Icon::Text(vec!["DOC".into(), "Text".into()]),
        "PID 1dof" | "PID 2dof" => Icon::Text(vec!["PID(s)".into()]),
        "Discrete PID Controller" => Icon::Text(vec!["PID(z)".into()]),
        "Band-Limited White Noise" => Icon::Strokes(vec![noise()]),
        "Saturation Dynamic" => Icon::Strokes(vec![vec![
            (0.1, 0.75),
            (0.35, 0.75),
            (0.65, 0.25),
            (0.9, 0.25),
        ]]),
        "Dead Zone Dynamic" => {
            Icon::Strokes(vec![vec![(0.1, 0.8), (0.4, 0.5), (0.6, 0.5), (0.9, 0.2)]])
        }
        // Zero up to the threshold's abscissa, then the input, then zero.
        "Wrap To Zero" => Icon::Strokes(vec![
            vec![(0.1, 0.8), (0.65, 0.25)],
            vec![(0.65, 0.25), (0.65, 0.8), (0.9, 0.8)],
        ]),
        // Odd curve with a jump at the origin: viscous slope plus Coulomb offset.
        "Coulomb and Viscous Friction" | "Coulomb & Viscous Friction" => Icon::Strokes(vec![
            vec![(0.1, 0.85), (0.5, 0.65), (0.5, 0.35), (0.9, 0.15)],
            vec![(0.1, 0.5), (0.9, 0.5)],
        ]),
        _ => Icon::Text(vec![source_type]),
    }
}

/// Extract literal text from common mask drawing commands: `disp('..')`,
/// `text(x, y, '..')` and `fprintf('..')`. Anything else is ignored.
pub fn mask_display_text(display: &str) -> Vec<String> {
    let mut out = Vec::new();
    for cmd in ["disp(", "fprintf(", "text("] {
        let mut rest = display;
        while let Some(i) = rest.find(cmd) {
            let before = &rest[..i];
            rest = &rest[i + cmd.len()..];
            if before.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            if let Some(lit) = first_string_literal(rest) {
                out.extend(lit.replace("\\n", "\n").lines().map(str::to_string));
            }
        }
    }
    out
}

fn first_string_literal(s: &str) -> Option<String> {
    let end = s.find(')').unwrap_or(s.len());
    let args = &s[..end];
    let start = args.find(['\'', '"'])?;
    let quote = args[start..].chars().next()?;
    let body = &s[start + 1..];
    let mut out = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == quote {
            if chars.peek() == Some(&quote) {
                out.push(quote);
                chars.next();
                continue;
            }
            return Some(out);
        }
        out.push(c);
    }
    None
}

/// Smallest font a parameter value shown on a block is drawn at; a value
/// needing less shows Simulink's placeholder instead.
const LEGIBLE: f64 = 7.0;

/// `value` as a block shows it: itself if it fits the block legibly
/// (within `fill` of its width and height), else `placeholder`, as Simulink
/// shows "-K-" for a gain too long to draw.
fn shown_value(block: &Block, value: String, fill: f64, placeholder: &str) -> String {
    let r = block.position;
    let lines: Vec<&str> = value.lines().collect();
    if fit_font(&lines, r.width() * fill, r.height() * fill, f64::MAX) < LEGIBLE {
        placeholder.to_string()
    } else {
        value
    }
}

fn non_empty(s: String, default: &str) -> String {
    if s.is_empty() {
        default.to_string()
    } else {
        s
    }
}

fn relop(op: &str) -> String {
    match op {
        "<=" => "≤".into(),
        ">=" => "≥".into(),
        "~=" => "≠".into(),
        "==" => "==".into(),
        other => other.into(),
    }
}

fn math_label(op: &str) -> String {
    match op {
        "exp" => "eᵘ".into(),
        "log" => "ln".into(),
        "10^u" => "10ᵘ".into(),
        "log10" => "log₁₀".into(),
        "magnitude^2" => "|u|²".into(),
        "square" => "u²".into(),
        "pow" => "uᵛ".into(),
        "conj" => "ū".into(),
        "reciprocal" => "1/u".into(),
        "hypot" => "hypot".into(),
        "rem" => "rem".into(),
        "mod" => "mod".into(),
        "transpose" => "uᵀ".into(),
        "hermitian" => "uᴴ".into(),
        other => other.into(),
    }
}

fn sqrt_label(op: &str) -> String {
    match op {
        "signedSqrt" => "signed\n√u".into(),
        "rSqrt" => "1/√u".into(),
        _ => "√u".into(),
    }
}

fn conversion_label(dtype: &str) -> String {
    let t = dtype.trim();
    if t.is_empty() || t.starts_with("Inherit") {
        "Convert".into()
    } else {
        t.into()
    }
}

/// Label for a round Sum: the signs, or `Σ` when there are many inputs.
fn sum_round_label(signs: &str) -> String {
    if signs.is_empty() || signs.len() > 3 {
        "Σ".into()
    } else {
        String::new()
    }
}

/// Render a MATLAB coefficient vector `[1 2 1]` as a polynomial in `var`.
/// Non-numeric expressions are shown verbatim.
pub fn poly(expr: &str, var: &str) -> String {
    let e = expr.trim();
    if e.is_empty() {
        return "1".into();
    }
    let inner = e.trim_start_matches('[').trim_end_matches(']');
    let coeffs: Option<Vec<f64>> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().ok())
        .collect();
    let Some(coeffs) = coeffs.filter(|c| !c.is_empty()) else {
        return e.to_string();
    };
    let n = coeffs.len() - 1;
    let mut out = String::new();
    for (i, &c) in coeffs.iter().enumerate() {
        if c == 0.0 {
            continue;
        }
        let power = n - i;
        let mag = c.abs();
        if !out.is_empty() {
            out.push_str(if c < 0.0 { "-" } else { "+" });
        } else if c < 0.0 {
            out.push('-');
        }
        if mag != 1.0 || power == 0 {
            out.push_str(&num(mag));
        }
        if power >= 1 {
            out.push_str(var);
        }
        if power >= 2 {
            out.push_str(&superscript(power));
        }
    }
    if out.is_empty() {
        "0".into()
    } else {
        out
    }
}

fn superscript(n: usize) -> String {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    n.to_string()
        .chars()
        .map(|c| DIGITS[c.to_digit(10).unwrap() as usize])
        .collect()
}

fn sine() -> Vec<(f64, f64)> {
    (0..=24)
        .map(|i| {
            let t = i as f64 / 24.0;
            (0.1 + 0.8 * t, 0.5 - 0.3 * (t * std::f64::consts::TAU).sin())
        })
        .collect()
}

/// A sine whose frequency rises across the icon.
fn chirp() -> Vec<(f64, f64)> {
    (0..=48)
        .map(|i| {
            let t = i as f64 / 48.0;
            let phase = std::f64::consts::TAU * (0.5 * t + 1.5 * t * t);
            (0.1 + 0.8 * t, 0.5 - 0.3 * phase.sin())
        })
        .collect()
}

fn noise() -> Vec<(f64, f64)> {
    const Y: [f64; 11] = [0.5, 0.3, 0.7, 0.4, 0.8, 0.25, 0.6, 0.35, 0.75, 0.45, 0.5];
    Y.iter()
        .enumerate()
        .map(|(i, &y)| (0.1 + 0.08 * i as f64, y))
        .collect()
}

/// Inport/Outport names inside a subsystem, ordered by port number, used to
/// label the subsystem's ports.
pub fn subsystem_port_labels(sys: &System, block_type: &str) -> Vec<(u32, String)> {
    let mut labels: Vec<(u32, String)> = sys
        .blocks
        .iter()
        .filter(|b| b.block_type == block_type)
        .map(|b| {
            let n = b
                .param("Port")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(1);
            (n, b.name.clone())
        })
        .collect();
    labels.sort();
    labels
}

/// Font size that fits `lines` in a `w × h` box, capped at `max`.
pub fn fit_font(lines: &[&str], w: f64, h: f64, max: f64) -> f64 {
    let widest = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let by_width = w * 0.9 / (widest * 0.6);
    let by_height = h * 0.85 / (lines.len().max(1) as f64 * 1.15);
    by_width.min(by_height).min(max)
}

/// Draw the outline and return the inner rectangle available for icons.
pub fn draw_shape(svg: &mut Svg, shape: Shape, block: &Block, fg: &str, bg: &str) -> Rect {
    let r = block.position;
    let (x, y, w, h) = (r.left, r.top, r.width().max(1.0), r.height().max(1.0));
    let stroke = [("stroke", fg.to_string()), ("fill", bg.to_string())];
    match shape {
        Shape::Rect => svg.leaf(
            "rect",
            &[
                ("x", num(x)),
                ("y", num(y)),
                ("width", num(w)),
                ("height", num(h)),
                stroke[0].clone(),
                stroke[1].clone(),
            ],
        ),
        Shape::Rounded => svg.leaf(
            "rect",
            &[
                ("x", num(x)),
                ("y", num(y)),
                ("width", num(w)),
                ("height", num(h)),
                ("rx", num(w.min(h) / 2.0)),
                stroke[0].clone(),
                stroke[1].clone(),
            ],
        ),
        Shape::Circle => svg.leaf(
            "ellipse",
            &[
                ("cx", num(x + w / 2.0)),
                ("cy", num(y + h / 2.0)),
                ("rx", num(w / 2.0)),
                ("ry", num(h / 2.0)),
                stroke[0].clone(),
                stroke[1].clone(),
            ],
        ),
        Shape::Triangle => {
            let pts = match block.orientation {
                Orientation::Right => vec![(x, y), (x + w, y + h / 2.0), (x, y + h)],
                Orientation::Left => vec![(x + w, y), (x, y + h / 2.0), (x + w, y + h)],
                Orientation::Down => vec![(x, y), (x + w / 2.0, y + h), (x + w, y)],
                Orientation::Up => vec![(x, y + h), (x + w / 2.0, y), (x + w, y + h)],
            };
            svg.leaf(
                "polygon",
                &[
                    ("points", points_attr(&pts)),
                    stroke[0].clone(),
                    stroke[1].clone(),
                ],
            );
        }
        Shape::Bar => svg.leaf(
            "rect",
            &[
                ("x", num(x)),
                ("y", num(y)),
                ("width", num(w)),
                ("height", num(h)),
                ("stroke", fg.to_string()),
                ("fill", fg.to_string()),
            ],
        ),
    }
    match shape {
        Shape::Triangle => match block.orientation {
            Orientation::Right => Rect::new(x, y + h * 0.25, x + w * 0.6, y + h * 0.75),
            Orientation::Left => Rect::new(x + w * 0.4, y + h * 0.25, x + w, y + h * 0.75),
            Orientation::Down => Rect::new(x + w * 0.25, y, x + w * 0.75, y + h * 0.6),
            Orientation::Up => Rect::new(x + w * 0.25, y + h * 0.4, x + w * 0.75, y + h),
        },
        Shape::Circle => Rect::new(x + w * 0.15, y + h * 0.15, x + w * 0.85, y + h * 0.85),
        _ => Rect::new(x + 2.0, y + 2.0, x + w - 2.0, y + h - 2.0),
    }
}

pub fn draw_icon(svg: &mut Svg, icon: &Icon, inner: Rect, fg: &str, max_font: f64) {
    let c = inner.center();
    match icon {
        Icon::None => {}
        Icon::Text(lines) => {
            let lines: Vec<&str> = lines
                .iter()
                .map(String::as_str)
                .filter(|l| !l.is_empty())
                .collect();
            if lines.is_empty() {
                return;
            }
            let size = fit_font(&lines, inner.width(), inner.height(), max_font);
            if size >= 3.0 {
                svg.text(
                    c.x,
                    c.y,
                    &lines,
                    size,
                    "middle",
                    true,
                    &[("fill", fg.to_string())],
                );
            }
        }
        Icon::Fraction(n, d) => {
            let size = fit_font(&[n, d], inner.width(), inner.height() / 1.2, max_font);
            if size < 3.0 {
                return;
            }
            let half = inner
                .width()
                .min(size * 0.6 * n.chars().count().max(d.chars().count()) as f64)
                / 2.0
                + 2.0;
            svg.leaf(
                "line",
                &[
                    ("x1", num(c.x - half)),
                    ("y1", num(c.y)),
                    ("x2", num(c.x + half)),
                    ("y2", num(c.y)),
                    ("stroke", fg.to_string()),
                ],
            );
            svg.text(
                c.x,
                c.y - size * 0.3,
                &[n],
                size,
                "middle",
                false,
                &[("fill", fg.to_string())],
            );
            svg.text(
                c.x,
                c.y + size * 1.05,
                &[d],
                size,
                "middle",
                false,
                &[("fill", fg.to_string())],
            );
        }
        Icon::Strokes(strokes) => {
            let (w, h) = (inner.width(), inner.height());
            for s in strokes {
                let pts: Vec<(f64, f64)> = s
                    .iter()
                    .map(|(u, v)| (inner.left + u * w, inner.top + v * h))
                    .collect();
                svg.leaf(
                    "polyline",
                    &[
                        ("points", points_attr(&pts)),
                        ("fill", "none".into()),
                        ("stroke", fg.to_string()),
                    ],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_switch_arm_follows_the_stored_position() {
        let arm = |params: &[(&str, &str)]| {
            let block = Block {
                id: "1".into(),
                block_type: "ManualSwitch".into(),
                name: "sw".into(),
                position: Rect::default(),
                orientation: Orientation::Right,
                mirrored: false,
                ports: Default::default(),
                parameters: params
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                mask: None,
                library_source: None,
                subsystem: None,
                style: Default::default(),
                interface: None,
            };
            match appearance(&block).icon {
                Icon::Strokes(strokes) => strokes[2][0].1,
                _ => panic!("the Manual Switch icon is drawn with strokes"),
            }
        };
        // Files store CurrentSetting (0 selects the lower input); the
        // programmatic sw wins when both are present.
        assert_eq!(arm(&[("CurrentSetting", "0")]), 0.7);
        assert_eq!(arm(&[("CurrentSetting", "1")]), 0.3);
        assert_eq!(arm(&[("sw", "0")]), 0.7);
        assert_eq!(arm(&[("sw", "1"), ("CurrentSetting", "0")]), 0.3);
    }

    #[test]
    fn long_values_show_placeholders() {
        let block = |ty: &str, key: &str, value: &str| Block {
            id: "1".into(),
            block_type: ty.into(),
            name: "b".into(),
            position: Rect::new(0.0, 0.0, 40.0, 30.0),
            orientation: Default::default(),
            mirrored: false,
            ports: Default::default(),
            parameters: [(key.to_string(), value.to_string())].into(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: Default::default(),
            interface: None,
        };
        let shown = |b: &Block| match appearance(b).icon {
            Icon::Text(lines) => lines.join("\n"),
            _ => String::new(),
        };
        assert_eq!(shown(&block("Gain", "Gain", "20")), "20");
        assert_eq!(
            shown(&block("Gain", "Gain", "[0 1 0 0; 0 -0.18 2.67 0]")),
            "-K-"
        );
        assert_eq!(shown(&block("Constant", "Value", "3.5")), "3.5");
        assert_eq!(
            shown(&block("Constant", "Value", "[1 2 3 4 5 6 7 8 9 10]")),
            "-C-"
        );
    }

    #[test]
    fn polynomials() {
        assert_eq!(poly("[1 2 1]", "s"), "s²+2s+1");
        assert_eq!(poly("[1]", "s"), "1");
        assert_eq!(poly("[2 0 -3]", "z"), "2z²-3");
        assert_eq!(poly("[-1 1]", "s"), "-s+1");
        assert_eq!(poly("num", "s"), "num");
    }

    #[test]
    fn mask_display() {
        assert_eq!(mask_display_text("disp('Hello')"), vec!["Hello"]);
        assert_eq!(mask_display_text("fprintf('a\\nb');"), vec!["a", "b"]);
        assert_eq!(mask_display_text("text(0.5, 0.5, 'It''s')"), vec!["It's"]);
        assert!(mask_display_text("plot([0 1],[0 1])").is_empty());
        assert!(mask_display_text("mydisp('x')").is_empty());
    }
}
