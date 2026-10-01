//! Render a Simulink diagram level to SVG.
//!
//! ```no_run
//! # let model: unlinked_model::Model = todo!();
//! let svg = unlinked_render::render_svg(&model, &["Controller"], &Default::default()).unwrap();
//! ```
//!
//! Every block is emitted as `<g class="block" data-sid=".." data-type="..">`
//! and subsystems additionally carry `data-subsystem="true"`, so an embedding
//! page can hit-test clicks without re-deriving geometry.
//! [`render_chart_svg`] draws Stateflow charts and MATLAB Function code.

mod chart;
mod color;
mod glyph;
mod route;
mod svg;
mod theme;

pub use chart::{render_chart_svg, render_chart_view_svg};
pub use theme::Theme;

use glyph::{appearance, draw_icon, draw_shape, subsystem_port_labels};
use std::collections::HashSet;
use svg::{num, points_attr, Svg};
use theme::Palette;
use unlinked_model::geometry::{port_anchor, port_on_outline, port_outward, port_side, Side};
use unlinked_model::{
    Annotation, Block, Model, NamePlacement, Orientation, PortKind, PortRef, Rect, System,
};

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub theme: Theme,
    /// Blank space around the diagram's bounding box.
    pub margin: f64,
    pub font_family: String,
    /// Largest font used for icon text.
    pub max_icon_font: f64,
    /// Add invisible pointer targets for editing: `circle.port-hit` on every
    /// signal port (`data-sid`, `data-kind`, `data-index`) and a wide
    /// `polyline.wire-hit` over each wire ending at a port (`data-dst-*`).
    pub hit_targets: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            theme: Theme::Light,
            margin: 30.0,
            font_family: "Helvetica, Arial, sans-serif".into(),
            max_icon_font: 12.0,
            hit_targets: false,
        }
    }
}

/// Upper bound on SVG elements emitted for one diagram level. The largest
/// corpus diagram needs a few thousand.
pub const MAX_ELEMENTS: usize = 200_000;

/// Unconnected-port markers drawn per port kind on one block.
const MAX_PORT_MARKERS: u32 = 64;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RenderError {
    #[error("no subsystem at path {0:?}")]
    NoSuchSystem(Vec<String>),
    #[error("no subchart with id {0:?}")]
    NoSuchView(String),
    #[error("diagram too large to render (more than {MAX_ELEMENTS} elements)")]
    TooLarge,
}

/// Render the system reached by following `path` (block names) from the
/// model root.
pub fn render_svg(
    model: &Model,
    path: &[&str],
    opts: &RenderOptions,
) -> Result<String, RenderError> {
    let sys = model
        .system_at(path)
        .ok_or_else(|| RenderError::NoSuchSystem(path.iter().map(|s| s.to_string()).collect()))?;
    render_system_svg(sys, opts)
}

const NAME_FONT: f64 = 10.0;
const LINE_LABEL_FONT: f64 = 9.0;

/// A block of text positioned the way [`Svg::text`] draws it, so the same
/// layout drives both drawing and the diagram's bounding box.
struct TextBox {
    x: f64,
    y: f64,
    lines: Vec<String>,
    size: f64,
    anchor: &'static str,
    v_center: bool,
}

impl TextBox {
    fn bounds(&self) -> Rect {
        let n = self.lines.len().max(1) as f64;
        let widest = self
            .lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0) as f64;
        let w = widest * self.size * 0.6;
        let lh = self.size * 1.15;
        let first = if self.v_center {
            self.y - lh * (n - 1.0) / 2.0 + self.size * 0.35
        } else {
            self.y
        };
        let (left, right) = match self.anchor {
            "middle" => (self.x - w / 2.0, self.x + w / 2.0),
            "end" => (self.x - w, self.x),
            _ => (self.x, self.x + w),
        };
        Rect::new(
            left,
            first - self.size,
            right,
            first + lh * (n - 1.0) + self.size * 0.3,
        )
    }

    fn draw(&self, s: &mut Svg, extra: &[(&str, String)]) {
        let refs: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        s.text(
            self.x,
            self.y,
            &refs,
            self.size,
            self.anchor,
            self.v_center,
            extra,
        );
    }
}

pub fn render_system_svg(sys: &System, opts: &RenderOptions) -> Result<String, RenderError> {
    let pal = opts.theme.palette();
    let routed: Vec<route::RoutedLine> = sys
        .lines
        .iter()
        .map(|l| route::route_line(sys, l))
        .collect();
    let names: Vec<Option<TextBox>> = sys.blocks.iter().map(name_layout).collect();
    let annotations: Vec<TextBox> = sys
        .annotations
        .iter()
        .filter_map(annotation_layout)
        .collect();
    let labels: Vec<Option<TextBox>> = routed.iter().map(label_layout).collect();

    let bounds = diagram_bounds(sys, &routed, &names, &annotations, &labels);
    let m = opts.margin;
    let (vx, vy, vw, vh) = (
        bounds.left - m,
        bounds.top - m,
        bounds.width() + 2.0 * m,
        bounds.height() + 2.0 * m,
    );

    let mut s = Svg::new();
    s.open(
        "svg",
        &[
            ("xmlns", "http://www.w3.org/2000/svg".into()),
            (
                "viewBox",
                format!("{} {} {} {}", num(vx), num(vy), num(vw), num(vh)),
            ),
            ("width", num(vw)),
            ("height", num(vh)),
            ("font-family", opts.font_family.clone()),
            ("stroke-linejoin", "round".into()),
        ],
    );
    s.leaf(
        "rect",
        &[
            ("x", num(vx)),
            ("y", num(vy)),
            ("width", num(vw)),
            ("height", num(vh)),
            ("fill", pal.canvas.into()),
            ("class", "canvas".into()),
        ],
    );

    for a in &annotations {
        a.draw(
            &mut s,
            &[("fill", pal.text.into()), ("class", "annotation".into())],
        );
    }

    let connected = connected_ports(sys);
    let mut order: Vec<usize> = (0..sys.blocks.len()).collect();
    order.sort_by_key(|&i| {
        sys.blocks[i]
            .param("ZOrder")
            .and_then(|z| z.trim().parse::<i64>().ok())
            .unwrap_or(0)
    });
    for i in order {
        draw_block(
            &mut s,
            &sys.blocks[i],
            names[i].as_ref(),
            &connected,
            opts,
            pal,
        );
        if s.elements() > MAX_ELEMENTS {
            return Err(RenderError::TooLarge);
        }
    }

    s.open("g", &[("class", "lines".into()), ("fill", "none".into())]);
    for (r, label) in routed.iter().zip(&labels) {
        draw_line(&mut s, r, label.as_ref(), pal);
        if s.elements() > MAX_ELEMENTS {
            return Err(RenderError::TooLarge);
        }
    }
    s.close("g");

    if opts.hit_targets {
        draw_hit_targets(&mut s, sys, &routed)?;
    }

    s.close("svg");
    if s.elements() > MAX_ELEMENTS {
        return Err(RenderError::TooLarge);
    }
    Ok(s.finish())
}

/// Union of block outlines, block names, line points, line labels and
/// annotations.
fn diagram_bounds(
    sys: &System,
    routed: &[route::RoutedLine],
    names: &[Option<TextBox>],
    annotations: &[TextBox],
    labels: &[Option<TextBox>],
) -> Rect {
    let mut acc: Option<Rect> = None;
    let mut add = |r: Rect| acc = Some(acc.map_or(r, |a| a.union(&r)));
    for b in &sys.blocks {
        let p = b.position;
        add(Rect::new(
            p.left - 8.0,
            p.top - 8.0,
            p.right + 8.0,
            p.bottom + 8.0,
        ));
    }
    for t in names.iter().chain(labels).flatten().chain(annotations) {
        add(t.bounds());
    }
    for r in routed {
        for w in &r.wires {
            for p in &w.points {
                add(Rect::new(p.x, p.y, p.x, p.y));
            }
        }
    }
    acc.unwrap_or(Rect::new(0.0, 0.0, 100.0, 100.0))
}

fn connected_ports(sys: &System) -> HashSet<(String, PortRef)> {
    let mut set = HashSet::new();
    for c in sys.connections() {
        set.insert((c.src.block.0.clone(), c.src.port));
        set.insert((c.dst.block.0.clone(), c.dst.port));
    }
    for l in &sys.lines {
        if let Some(src) = &l.src {
            set.insert((src.block.0.clone(), src.port));
        }
    }
    set
}

const ALL_KINDS: [PortKind; 9] = [
    PortKind::In,
    PortKind::Out,
    PortKind::Enable,
    PortKind::Trigger,
    PortKind::State,
    PortKind::LConn,
    PortKind::RConn,
    PortKind::IfAction,
    PortKind::Reset,
];

fn draw_block(
    s: &mut Svg,
    b: &Block,
    name: Option<&TextBox>,
    connected: &HashSet<(String, PortRef)>,
    opts: &RenderOptions,
    pal: &Palette,
) {
    let bg = pal.block_bg(b.style.background.as_deref());
    let fg = color::readable(
        &pal.block_fg(b.style.foreground.as_deref()),
        &bg,
        "#1a1b26",
        "#c0caf5",
    );
    let mut attrs = vec![
        ("class", "block".to_string()),
        ("data-sid", b.id.0.clone()),
        ("data-type", b.block_type.clone()),
        ("data-name", b.name.clone()),
    ];
    if b.subsystem.is_some() {
        attrs.push(("data-subsystem", "true".into()));
    }
    s.open("g", &attrs);
    s.open("title", &[]);
    s.raw(&svg::escape(&format!("{} ({})", b.name, b.display_type())));
    s.close("title");

    if b.style.drop_shadow {
        let r = b.position;
        s.leaf(
            "rect",
            &[
                ("x", num(r.left + 3.0)),
                ("y", num(r.top + 3.0)),
                ("width", num(r.width())),
                ("height", num(r.height())),
                ("fill", pal.shadow.into()),
            ],
        );
    }

    let look = appearance(b);
    let inner = draw_shape(s, look.shape, b, &fg, &bg);
    draw_icon(s, &look.icon, inner, &fg, opts.max_icon_font);
    if b.block_type == "Sum" {
        draw_sum_signs(s, b, &fg);
    }
    if let Some(element) = b.interface.as_ref().and_then(|i| i.element.as_deref()) {
        draw_bus_element_label(s, b, element, pal);
    }

    if let Some(sub) = &b.subsystem {
        draw_subsystem_port_labels(s, b, sub, &fg);
    }

    for kind in ALL_KINDS {
        for index in 1..=b.ports.count(kind).min(MAX_PORT_MARKERS) {
            let port = PortRef { kind, index };
            if !connected.contains(&(b.id.0.clone(), port)) {
                draw_port_chevron(s, b, port, &fg);
            }
        }
    }

    if let Some(name) = name {
        name.draw(s, &[("fill", pal.text.into()), ("class", "name".into())]);
    }
    s.close("g");
}

/// Bus element ports (`In Bus Element` / `Out Bus Element`) show the
/// selected element beside their small marker.
fn draw_bus_element_label(s: &mut Svg, b: &Block, element: &str, pal: &Palette) {
    let r = b.position;
    let c = r.center();
    let text = [element];
    let fill = [("fill", pal.muted.to_string())];
    match (b.block_type.as_str(), b.orientation, b.mirrored) {
        ("Inport", Orientation::Right, _) | ("Outport", Orientation::Left, _) => {
            s.text(r.left - 4.0, c.y, &text, 8.0, "end", true, &fill)
        }
        _ => s.text(r.right + 4.0, c.y, &text, 8.0, "start", true, &fill),
    }
}

/// Place each input's `+`/`-` just inside its port.
fn draw_sum_signs(s: &mut Svg, b: &Block, fg: &str) {
    let signs: Vec<char> = b
        .param("Inputs")
        .unwrap_or("|++")
        .chars()
        .filter(|c| matches!(c, '+' | '-'))
        .collect();
    let size = 9.0_f64
        .min(b.position.height() / 2.5)
        .min(b.position.width() / 2.5);
    if size < 4.0 || signs.len() > MAX_PORT_MARKERS as usize {
        return;
    }
    for (i, sign) in signs.iter().enumerate() {
        let port = PortRef {
            kind: PortKind::In,
            index: i as u32 + 1,
        };
        if port.index > b.ports.inputs {
            break;
        }
        let p = port_on_outline(b, port);
        let (dx, dy) = port_outward(b, port);
        let inset = size * 0.8;
        let label = if *sign == '-' { "−" } else { "+" };
        s.text(
            p.x - dx * inset,
            p.y - dy * inset,
            &[label],
            size,
            "middle",
            true,
            &[("fill", fg.to_string())],
        );
    }
}

/// Open chevron marking an unconnected port.
fn draw_port_chevron(s: &mut Svg, b: &Block, port: PortRef, fg: &str) {
    let tip_base = port_on_outline(b, port);
    let (dx, dy) = port_outward(b, port);
    // Inputs point into the block, outputs point away from it.
    let into = matches!(port.kind, PortKind::Out | PortKind::State | PortKind::RConn);
    let (fx, fy) = if into { (dx, dy) } else { (-dx, -dy) };
    let base = if into { tip_base } else { port_anchor(b, port) };
    let tip = (base.x + fx * 5.0, base.y + fy * 5.0);
    let (px, py) = (-fy * 3.5, fx * 3.5);
    let pts = [(base.x + px, base.y + py), tip, (base.x - px, base.y - py)];
    s.leaf(
        "polyline",
        &[
            ("points", points_attr(&pts)),
            ("fill", "none".into()),
            ("stroke", fg.to_string()),
            ("class", "port".into()),
        ],
    );
}

fn draw_subsystem_port_labels(s: &mut Svg, b: &Block, sub: &System, fg: &str) {
    let r = b.position;
    let size = 8.0_f64.min(r.height() / 2.0);
    if size < 4.0 {
        return;
    }
    for (kind, block_type) in [(PortKind::In, "Inport"), (PortKind::Out, "Outport")] {
        for (n, name) in subsystem_port_labels(sub, block_type) {
            if n == 0 || n > b.ports.count(kind) {
                continue;
            }
            let port = PortRef { kind, index: n };
            let p = port_on_outline(b, port);
            let side = port_side(b.orientation, b.mirrored, kind);
            let label = name.replace('\n', " ");
            let max_chars = ((r.width() / 2.0 - 3.0) / (size * 0.6)).max(1.0) as usize;
            let label = truncate(&label, max_chars);
            let fill = [("fill", fg.to_string())];
            match side {
                Side::Left => s.text(p.x + 3.0, p.y, &[&label], size, "start", true, &fill),
                Side::Right => s.text(p.x - 3.0, p.y, &[&label], size, "end", true, &fill),
                Side::Top => s.text(
                    p.x,
                    p.y + size + 2.0,
                    &[&label],
                    size,
                    "middle",
                    false,
                    &fill,
                ),
                Side::Bottom => s.text(p.x, p.y - 3.0, &[&label], size, "middle", false, &fill),
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn name_layout(b: &Block) -> Option<TextBox> {
    if !b.style.show_name || b.name.is_empty() {
        return None;
    }
    let r = b.position;
    let lines: Vec<String> = b.name.lines().map(str::to_string).collect();
    let size = b
        .style
        .font_size
        .filter(|f| *f > 0.0 && *f < 72.0)
        .unwrap_or(NAME_FONT);
    let alternate = b.style.name_placement == NamePlacement::Alternate;
    let lh = size * 1.15;
    let n = lines.len() as f64;
    let (x, y, anchor, v_center) = match (b.orientation, alternate) {
        (Orientation::Right | Orientation::Left, false) => {
            (r.center().x, r.bottom + size + 2.0, "middle", false)
        }
        (Orientation::Right | Orientation::Left, true) => {
            (r.center().x, r.top - 4.0 - lh * (n - 1.0), "middle", false)
        }
        (Orientation::Up | Orientation::Down, false) => {
            (r.right + 4.0, r.center().y, "start", true)
        }
        (Orientation::Up | Orientation::Down, true) => (r.left - 4.0, r.center().y, "end", true),
    };
    Some(TextBox {
        x,
        y,
        lines,
        size,
        anchor,
        v_center,
    })
}

fn label_layout(r: &route::RoutedLine) -> Option<TextBox> {
    let (p, label) = r.label.as_ref()?;
    Some(TextBox {
        x: p.x,
        y: p.y - 4.0,
        lines: vec![label.clone()],
        size: LINE_LABEL_FONT,
        anchor: "middle",
        v_center: false,
    })
}

/// Signal port kinds that can be wired in the editor.
const WIRABLE: [PortKind; 7] = [
    PortKind::In,
    PortKind::Out,
    PortKind::Enable,
    PortKind::Trigger,
    PortKind::State,
    PortKind::IfAction,
    PortKind::Reset,
];

/// Transparent pointer targets drawn above everything else; see
/// [`RenderOptions::hit_targets`].
fn draw_hit_targets(
    s: &mut Svg,
    sys: &System,
    routed: &[route::RoutedLine],
) -> Result<(), RenderError> {
    let budget = |s: &Svg| {
        if s.elements() > MAX_ELEMENTS {
            Err(RenderError::TooLarge)
        } else {
            Ok(())
        }
    };
    let endpoint_attrs = |prefix: &str, ep: &unlinked_model::Endpoint| {
        [
            (format!("data-{prefix}sid"), ep.block.0.clone()),
            (
                format!("data-{prefix}kind"),
                ep.port.kind.token().to_string(),
            ),
            (format!("data-{prefix}index"), ep.port.index.to_string()),
        ]
    };
    s.open(
        "g",
        &[
            ("class", "wire-hits".into()),
            ("fill", "none".into()),
            ("stroke", "transparent".into()),
            ("stroke-width", "9".into()),
        ],
    );
    for w in routed.iter().flat_map(|r| &r.wires) {
        let Some(dst) = &w.dst else {
            continue;
        };
        let pts: Vec<(f64, f64)> = w.points.iter().map(|p| (p.x, p.y)).collect();
        let data = endpoint_attrs("dst-", dst);
        let mut attrs = vec![
            ("class", "wire-hit".to_string()),
            ("points", points_attr(&pts)),
        ];
        attrs.extend(data.iter().map(|(k, v)| (k.as_str(), v.clone())));
        s.leaf("polyline", &attrs);
        budget(s)?;
    }
    s.close("g");

    s.open(
        "g",
        &[
            ("class", "port-hits".into()),
            ("fill", "transparent".into()),
        ],
    );
    for b in &sys.blocks {
        for kind in WIRABLE {
            for index in 1..=b.ports.count(kind).min(MAX_PORT_MARKERS) {
                let port = PortRef { kind, index };
                let at = unlinked_model::geometry::port_anchor(b, port);
                let ep = unlinked_model::Endpoint {
                    block: b.id.clone(),
                    port,
                };
                let data = endpoint_attrs("", &ep);
                let mut attrs = vec![
                    ("class", "port-hit".to_string()),
                    ("cx", num(at.x)),
                    ("cy", num(at.y)),
                    ("r", "5".to_string()),
                ];
                attrs.extend(data.iter().map(|(k, v)| (k.as_str(), v.clone())));
                s.leaf("circle", &attrs);
            }
        }
        budget(s)?;
    }
    s.close("g");
    Ok(())
}

fn draw_line(s: &mut Svg, r: &route::RoutedLine, label: Option<&TextBox>, pal: &Palette) {
    for w in &r.wires {
        let pts: Vec<(f64, f64)> = w.points.iter().map(|p| (p.x, p.y)).collect();
        let color = if w.dangling { pal.error } else { pal.line };
        let mut attrs = vec![
            ("points", points_attr(&pts)),
            ("stroke", color.to_string()),
            ("class", "signal".to_string()),
        ];
        if w.dangling {
            attrs.push(("stroke-dasharray", "4 3".into()));
        }
        s.leaf("polyline", &attrs);
        if let Some((tip, (dx, dy))) = w.arrow {
            let len = 7.0;
            let half = 3.5;
            let base = (tip.x - dx * len, tip.y - dy * len);
            let (px, py) = (-dy * half, dx * half);
            let tri = [
                (tip.x, tip.y),
                (base.0 + px, base.1 + py),
                (base.0 - px, base.1 - py),
            ];
            s.leaf(
                "polygon",
                &[
                    ("points", points_attr(&tri)),
                    ("fill", color.into()),
                    ("stroke", "none".into()),
                ],
            );
        }
    }
    for j in &r.junctions {
        s.leaf(
            "circle",
            &[
                ("cx", num(j.x)),
                ("cy", num(j.y)),
                ("r", "2.5".into()),
                ("fill", pal.line.into()),
            ],
        );
    }
    if let Some(label) = label {
        label.draw(s, &[("fill", pal.text.into()), ("stroke", "none".into())]);
    }
}

/// Annotation text with rich-text (HTML) markup reduced to plain lines.
fn annotation_lines(a: &Annotation) -> Vec<String> {
    let text = if a.rich_text {
        strip_html(&a.text)
    } else {
        a.text.clone()
    };
    text.lines().map(|l| l.trim_end().to_string()).collect()
}

fn annotation_layout(a: &Annotation) -> Option<TextBox> {
    let lines = annotation_lines(a);
    if lines.iter().all(|l| l.is_empty()) {
        return None;
    }
    let p = a.position;
    let size = a
        .properties
        .get("FontSize")
        .and_then(|f| f.trim().parse::<f64>().ok())
        .filter(|f| *f > 0.0 && *f < 72.0)
        .unwrap_or(10.0);
    let boxed = p.right > p.left;
    let (x, anchor) = match a.properties.get("HorizontalAlignment").map(String::as_str) {
        Some("center") if boxed => (p.center().x, "middle"),
        Some("right") if boxed => (p.right, "end"),
        Some("center") => (p.left, "middle"),
        _ => (p.left, "start"),
    };
    let y = if boxed { p.top + size } else { p.top };
    Some(TextBox {
        x,
        y,
        lines,
        size,
        anchor,
        v_center: false,
    })
}

fn strip_html(html: &str) -> String {
    let body = match (html.find("<body"), html.rfind("</body>")) {
        (Some(start), Some(end)) if start < end => &html[start..end],
        _ => html,
    };
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for c in body.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let t = tag.trim_start_matches('/').to_ascii_lowercase();
                if t.starts_with("p")
                    || t.starts_with("br")
                    || t.starts_with("div")
                    || t.starts_with("li")
                {
                    out.push('\n');
                }
            }
            _ if in_tag => tag.push(c),
            '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use unlinked_model::{BlockStyle, PortCounts};

    fn block(name: &str, pos: Rect) -> Block {
        Block {
            id: "1".into(),
            block_type: "Gain".into(),
            name: name.into(),
            position: pos,
            orientation: Orientation::Right,
            mirrored: false,
            ports: PortCounts::from_slice(&[1, 1]),
            parameters: BTreeMap::new(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: BlockStyle::default(),
            interface: None,
        }
    }

    #[test]
    fn html_is_stripped() {
        let html = "<!DOCTYPE HTML><html><head><style>p{}</style></head><body><p>Hello &amp; welcome</p><p>Line 2</p></body></html>";
        assert_eq!(strip_html(html), "Hello & welcome\nLine 2");
    }

    #[test]
    fn missing_system_errors() {
        let model = Model {
            name: "m".into(),
            source: unlinked_model::SourceFormat::Slx,
            simulink_version: None,
            config: Default::default(),
            root: System::default(),
            workspace: Default::default(),
            charts: Vec::new(),
        };
        assert!(render_svg(&model, &[], &RenderOptions::default()).is_ok());
        assert_eq!(
            render_svg(&model, &["nope"], &RenderOptions::default()),
            Err(RenderError::NoSuchSystem(vec!["nope".into()]))
        );
    }

    #[test]
    fn long_names_widen_the_view() {
        let name = "x".repeat(100);
        let sys = System {
            blocks: vec![block(&name, Rect::new(0.0, 0.0, 40.0, 40.0))],
            ..Default::default()
        };
        let svg = render_system_svg(&sys, &RenderOptions::default()).unwrap();
        let view = svg
            .split("viewBox=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let width: f64 = view.split(' ').nth(2).unwrap().parse().unwrap();
        assert!(width > 100.0 * NAME_FONT * 0.6, "viewBox {view}");
    }

    #[test]
    fn hit_targets_name_ports_and_wire_destinations() {
        let mut b = block("b", Rect::new(100.0, 0.0, 140.0, 40.0));
        b.id = "2".into();
        let ep = |id: &str, kind| unlinked_model::Endpoint {
            block: id.into(),
            port: PortRef { kind, index: 1 },
        };
        let sys = System {
            blocks: vec![block("a", Rect::new(0.0, 0.0, 40.0, 40.0)), b],
            lines: vec![unlinked_model::Line {
                src: Some(ep("1", PortKind::Out)),
                dst: Some(ep("2", PortKind::In)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let plain = render_system_svg(&sys, &RenderOptions::default()).unwrap();
        assert!(!plain.contains("port-hit") && !plain.contains("wire-hit"));
        let opts = RenderOptions {
            hit_targets: true,
            ..Default::default()
        };
        let svg = render_system_svg(&sys, &opts).unwrap();
        assert_eq!(svg.matches("class=\"port-hit\"").count(), 4, "{svg}");
        assert!(svg.contains(
            "class=\"port-hit\" cx=\"145\" cy=\"20\" r=\"5\" data-sid=\"2\" data-kind=\"out\" data-index=\"1\""
        ), "{svg}");
        assert!(
            svg.contains("data-dst-sid=\"2\" data-dst-kind=\"in\" data-dst-index=\"1\""),
            "{svg}"
        );
    }

    #[test]
    fn dark_theme_uses_tokyo_night() {
        let sys = System {
            blocks: vec![block("g", Rect::new(0.0, 0.0, 40.0, 40.0))],
            ..Default::default()
        };
        let opts = RenderOptions {
            theme: Theme::Dark,
            ..Default::default()
        };
        let svg = render_system_svg(&sys, &opts).unwrap();
        assert!(svg.contains("fill=\"#1a1b26\""));
        assert!(svg.contains("stroke=\"#c0caf5\""));
        assert!(!svg.contains("#ffffff"));
    }

    #[test]
    fn annotation_tspans_count_toward_budget() {
        let annotation = Annotation {
            text: "x\n".repeat(MAX_ELEMENTS + 1),
            ..Default::default()
        };
        let sys = System {
            annotations: vec![annotation],
            ..Default::default()
        };
        assert_eq!(
            render_system_svg(&sys, &RenderOptions::default()),
            Err(RenderError::TooLarge)
        );
    }

    #[test]
    fn huge_diagrams_are_rejected() {
        let mut b = block("g", Rect::new(0.0, 0.0, 40.0, 40.0));
        b.ports = PortCounts::from_slice(&[1024, 1024, 1024, 1024, 1024, 1024, 1024, 1024, 1024]);
        let sys = System {
            blocks: (0..400)
                .map(|i| {
                    let mut b = b.clone();
                    b.id = unlinked_model::BlockId(i.to_string());
                    b
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            render_system_svg(&sys, &RenderOptions::default()),
            Err(RenderError::TooLarge)
        );
    }
}
