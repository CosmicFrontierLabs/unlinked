//! Render a Stateflow chart view, or a MATLAB Function block's code, to SVG.
//!
//! States are emitted as `<g class="state" data-sid=".." data-name="..">`;
//! subcharted states that have their own view also carry
//! `data-subchart="true"` so a page can open them.

use crate::svg::{escape, num, points_attr, Svg};
use crate::theme::Palette;
use crate::{RenderError, RenderOptions, MAX_ELEMENTS};
use std::borrow::Cow;
use unlinked_model::{Chart, ChartKind, JunctionKind, Point, Rect, State, StateKind, Transition};

const STATE_FONT: f64 = 11.0;
const TRANSITION_FONT: f64 = 10.0;
const CODE_FONT: f64 = 12.0;
const CODE_FAMILY: &str = "Menlo, Consolas, 'DejaVu Sans Mono', monospace";
/// Longest code listing drawn; the rest is summarised in a final line.
const MAX_CODE_LINES: usize = 5000;
/// Characters drawn per code line; longer lines end in an ellipsis.
const MAX_CODE_LINE_CHARS: usize = 400;
/// Label lines drawn per state; the box clips anything taller anyway.
const MAX_STATE_LINES: usize = 200;
/// Label lines drawn per transition.
const MAX_TRANSITION_LINES: usize = 50;
/// Characters drawn per label line.
const MAX_LABEL_LINE_CHARS: usize = 200;
/// Characters of a state's label repeated in its tooltip.
const MAX_TITLE_CHARS: usize = 1000;
/// Upper bound on the markup of one chart view, so long text cannot slip
/// past the element budget.
const MAX_CHART_BYTES: usize = 16 * 1024 * 1024;

/// `s` cut to `max` characters, ending in an ellipsis when cut.
fn clip(s: &str, max: usize) -> Cow<'_, str> {
    match s.char_indices().nth(max) {
        None => Cow::Borrowed(s),
        Some((i, _)) => Cow::Owned(format!("{}…", &s[..i])),
    }
}

/// At most `max_lines` lines of `text`, each clipped to `max_chars`.
/// Blank lines become a space: browsers skip empty tspans, and with them
/// their line advance.
fn clip_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<Cow<'_, str>> {
    text.lines()
        .take(max_lines)
        .map(|l| {
            if l.trim().is_empty() {
                Cow::Borrowed(" ")
            } else {
                clip(l, max_chars)
            }
        })
        .collect()
}

fn over_budget(s: &Svg) -> bool {
    s.elements() > MAX_ELEMENTS || s.bytes() > MAX_CHART_BYTES
}

/// Render the chart's top-level view. MATLAB Function blocks render their
/// code as a listing.
pub fn render_chart_svg(chart: &Chart, opts: &RenderOptions) -> Result<String, RenderError> {
    render_chart_view_svg(chart, None, opts)
}

/// Render one view of a chart: `None` for the chart itself, or the id of a
/// subcharted state (see [`Chart::view_at`]).
pub fn render_chart_view_svg(
    chart: &Chart,
    view: Option<&str>,
    opts: &RenderOptions,
) -> Result<String, RenderError> {
    if let Some(v) = view {
        if !chart.subchart_ids().contains(v) {
            return Err(RenderError::NoSuchView(v.to_string()));
        }
    }
    let s = match (&chart.kind, &chart.script) {
        (ChartKind::MatlabFunction, Some(script)) if view.is_none() => {
            code_listing(chart, script, opts)
        }
        _ => diagram(chart, view, opts),
    };
    if over_budget(&s) {
        return Err(RenderError::TooLarge);
    }
    Ok(s.finish())
}

fn open_svg(s: &mut Svg, bounds: Rect, opts: &RenderOptions, pal: &Palette) {
    let m = opts.margin;
    let (vx, vy, vw, vh) = (
        bounds.left - m,
        bounds.top - m,
        bounds.width() + 2.0 * m,
        bounds.height() + 2.0 * m,
    );
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
}

fn text_width(line: &str, size: f64) -> f64 {
    line.chars().count() as f64 * size * 0.6
}

fn code_listing(chart: &Chart, script: &str, opts: &RenderOptions) -> Svg {
    let pal = opts.theme.palette();
    let lines: Vec<String> = clip_lines(script, MAX_CODE_LINES, MAX_CODE_LINE_CHARS)
        .into_iter()
        .map(|l| l.trim_end().replace('\t', "    "))
        .map(|l| if l.is_empty() { " ".to_string() } else { l })
        .collect();
    let shown = lines.len();
    let hidden = script.lines().skip(shown).count();
    let mut code: Vec<&str> = lines.iter().map(String::as_str).collect();
    let more = format!("… {hidden} more lines");
    if hidden > 0 {
        code.push(&more);
    }
    let numbers: Vec<String> = (1..=shown).map(|n| n.to_string()).collect();
    let number_refs: Vec<&str> = numbers.iter().map(String::as_str).collect();

    let lh = CODE_FONT * 1.15;
    let title = format!(
        "MATLAB Function · {}",
        clip(&chart.name, MAX_LABEL_LINE_CHARS).replace('\n', " ")
    );
    let gutter = text_width(&shown.to_string(), CODE_FONT) + 12.0;
    let widest = code
        .iter()
        .map(|l| text_width(l, CODE_FONT))
        .fold(text_width(&title, CODE_FONT), f64::max);
    let top = 2.0 * lh;
    let bounds = Rect::new(
        0.0,
        0.0,
        gutter + widest + 16.0,
        top + lh * code.len() as f64 + 8.0,
    );

    let mut s = Svg::new();
    open_svg(&mut s, bounds, opts, pal);
    s.leaf(
        "rect",
        &[
            ("x", num(bounds.left)),
            ("y", num(bounds.top)),
            ("width", num(bounds.width())),
            ("height", num(bounds.height())),
            ("rx", "4".into()),
            ("fill", pal.block_fill.into()),
            ("stroke", pal.muted.into()),
        ],
    );
    s.text(
        8.0,
        lh,
        &[&title],
        CODE_FONT,
        "start",
        false,
        &[("fill", pal.text.into()), ("font-weight", "bold".into())],
    );
    let pre = [
        ("font-family", CODE_FAMILY.to_string()),
        ("xml:space", "preserve".to_string()),
        ("style", "white-space:pre".to_string()),
    ];
    let mut attrs = pre.to_vec();
    attrs.extend([
        ("fill", pal.muted.to_string()),
        ("class", "line-numbers".to_string()),
    ]);
    s.text(
        gutter - 6.0,
        top + lh,
        &number_refs,
        CODE_FONT,
        "end",
        false,
        &attrs,
    );
    let mut attrs = pre.to_vec();
    attrs.extend([
        ("fill", pal.text.to_string()),
        ("class", "code".to_string()),
    ]);
    s.text(
        gutter + 4.0,
        top + lh,
        &code,
        CODE_FONT,
        "start",
        false,
        &attrs,
    );
    s.close("svg");
    s
}

/// Label text of a transition, laid out at its label position or beside
/// its midpoint.
fn transition_label(t: &Transition) -> Option<(Point, Vec<Cow<'_, str>>)> {
    let lines: Vec<Cow<str>> = t
        .label
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(MAX_TRANSITION_LINES)
        .map(|l| clip(l, MAX_LABEL_LINE_CHARS))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let at = match t.label_position {
        Some(p) => Point::new(p.x, p.y + TRANSITION_FONT),
        None => {
            let mid = t.points.get(t.points.len() / 2)?;
            Point::new(mid.x + 4.0, mid.y - 4.0)
        }
    };
    Some((at, lines))
}

fn diagram(chart: &Chart, view: Option<&str>, opts: &RenderOptions) -> Svg {
    let pal = opts.theme.palette();
    let mut states: Vec<&State> = chart
        .states
        .iter()
        .filter(|s| chart.in_view(s.subviewer.as_deref(), view))
        .filter(|s| s.position.width() > 0.0 && s.position.height() > 0.0)
        .collect();
    // Enclosing states first so nested ones draw on top.
    states.sort_by(|a, b| {
        let area = |s: &State| s.position.width() * s.position.height();
        area(b).total_cmp(&area(a))
    });
    let junctions: Vec<_> = chart
        .junctions
        .iter()
        .filter(|j| chart.in_view(j.subviewer.as_deref(), view))
        .collect();
    let transitions: Vec<&Transition> = chart
        .transitions
        .iter()
        .filter(|t| chart.in_view(t.subviewer.as_deref(), view) && t.points.len() >= 2)
        .collect();

    let mut acc: Option<Rect> = None;
    let mut add = |r: Rect| acc = Some(acc.map_or(r, |a| a.union(&r)));
    for st in &states {
        add(st.position);
    }
    for j in &junctions {
        add(j.position);
    }
    for t in &transitions {
        for p in &t.points {
            add(Rect::new(p.x - 3.0, p.y - 3.0, p.x + 3.0, p.y + 3.0));
        }
        if let Some((at, lines)) = transition_label(t) {
            let w = lines
                .iter()
                .map(|l| text_width(l, TRANSITION_FONT))
                .fold(0.0, f64::max);
            let h = TRANSITION_FONT * 1.15 * lines.len() as f64;
            add(Rect::new(at.x, at.y - TRANSITION_FONT, at.x + w, at.y + h));
        }
    }
    let note = acc.is_none().then(|| match &chart.kind {
        ChartKind::MatlabFunction => "MATLAB Function (no code)".to_string(),
        ChartKind::TruthTable => "Truth table".to_string(),
        ChartKind::StateChart => "Empty chart".to_string(),
        ChartKind::Other(t) => format!("Stateflow {t}"),
    });
    let bounds = acc.unwrap_or(Rect::new(0.0, 0.0, 200.0, 40.0));

    let mut s = Svg::new();
    open_svg(&mut s, bounds, opts, pal);
    if let Some(note) = note {
        s.text(
            100.0,
            20.0,
            &[&note],
            STATE_FONT,
            "middle",
            true,
            &[("fill", pal.muted.into())],
        );
    }

    let clip_prefix: String = chart
        .id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let subcharts = chart.subchart_ids();
    for (i, st) in states.iter().enumerate() {
        let subchart = subcharts.contains(st.id.as_str());
        draw_state(&mut s, st, subchart, &format!("sf{clip_prefix}-{i}"), pal);
        if over_budget(&s) {
            return s;
        }
    }

    s.open(
        "g",
        &[
            ("class", "transitions".into()),
            ("fill", "none".into()),
            ("stroke", pal.line.into()),
        ],
    );
    for t in &transitions {
        draw_transition(&mut s, t, pal);
        if over_budget(&s) {
            return s;
        }
    }
    s.close("g");

    for j in &junctions {
        let c = j.position.center();
        let r = (j.position.width() / 2.0).max(2.0);
        s.open(
            "g",
            &[("class", "junction".into()), ("data-sid", j.id.clone())],
        );
        s.leaf(
            "circle",
            &[
                ("cx", num(c.x)),
                ("cy", num(c.y)),
                ("r", num(r)),
                ("fill", pal.canvas.into()),
                ("stroke", pal.line.into()),
            ],
        );
        if j.kind == JunctionKind::History {
            s.text(
                c.x,
                c.y,
                &["H"],
                r * 1.3,
                "middle",
                true,
                &[("fill", pal.text.into())],
            );
        }
        s.close("g");
        if over_budget(&s) {
            return s;
        }
    }
    s.close("svg");
    s
}

/// Free-text note: its (possibly rich) text, with no outline.
fn draw_note(s: &mut Svg, st: &State, pal: &Palette) {
    let text = if st.label.trim_start().starts_with('<') {
        Cow::Owned(crate::strip_html(&st.label))
    } else {
        Cow::Borrowed(st.label.as_str())
    };
    let lines = clip_lines(&text, MAX_STATE_LINES, MAX_LABEL_LINE_CHARS);
    let lines: Vec<&str> = lines.iter().map(|l| l.as_ref()).collect();
    let r = st.position;
    s.open(
        "g",
        &[("class", "note".into()), ("data-sid", st.id.clone())],
    );
    s.text(
        r.left + 2.0,
        r.top + STATE_FONT,
        &lines,
        STATE_FONT,
        "start",
        false,
        &[("fill", pal.muted.into()), ("font-style", "italic".into())],
    );
    s.close("g");
}

fn draw_state(s: &mut Svg, st: &State, subchart: bool, clip_id: &str, pal: &Palette) {
    if st.kind == StateKind::Note {
        return draw_note(s, st, pal);
    }
    let r = st.position;
    let mut attrs = vec![
        ("class", "state".to_string()),
        ("data-sid", st.id.clone()),
        (
            "data-name",
            clip(st.name(), MAX_LABEL_LINE_CHARS).into_owned(),
        ),
    ];
    if subchart {
        attrs.push(("data-subchart", "true".into()));
    }
    s.open("g", &attrs);
    s.open("title", &[]);
    let hint = if subchart {
        " (double-click to open)"
    } else {
        ""
    };
    s.raw(&escape(&format!(
        "{}{hint}",
        clip(&st.label, MAX_TITLE_CHARS)
    )));
    s.close("title");

    let radius = 8.0_f64.min(r.width() / 4.0).min(r.height() / 4.0);
    let fill = match st.kind {
        StateKind::Group => "none",
        _ => pal.block_fill,
    };
    let mut rect = vec![
        ("x", num(r.left)),
        ("y", num(r.top)),
        ("width", num(r.width())),
        ("height", num(r.height())),
        ("rx", num(radius)),
        ("fill", fill.to_string()),
        ("stroke", pal.fg.to_string()),
    ];
    match st.kind {
        StateKind::And => rect.push(("stroke-dasharray", "6 3".into())),
        StateKind::Group => rect.push(("stroke", pal.muted.to_string())),
        _ => {}
    }
    if subchart {
        rect.push(("stroke-width", "2".into()));
    }
    s.leaf("rect", &rect);

    s.open("clipPath", &[("id", clip_id.to_string())]);
    s.leaf(
        "rect",
        &[
            ("x", num(r.left)),
            ("y", num(r.top)),
            ("width", num(r.width())),
            ("height", num(r.height())),
        ],
    );
    s.close("clipPath");
    s.open("g", &[("clip-path", format!("url(#{clip_id})"))]);
    let lines = clip_lines(&st.label, MAX_STATE_LINES, MAX_LABEL_LINE_CHARS);
    let lines: Vec<&str> = lines.iter().map(|l| l.as_ref()).collect();
    let (x, y) = (r.left + 5.0, r.top + STATE_FONT + 3.0);
    if let Some((first, rest)) = lines.split_first() {
        s.text(
            x,
            y,
            &[first],
            STATE_FONT,
            "start",
            false,
            &[
                ("fill", pal.text.into()),
                ("font-weight", "bold".into()),
                ("class", "state-name".into()),
            ],
        );
        s.text(
            x,
            y + STATE_FONT * 1.15,
            rest,
            STATE_FONT,
            "start",
            false,
            &[
                ("fill", pal.text.into()),
                ("xml:space", "preserve".into()),
                ("style", "white-space:pre".into()),
                ("class", "state-actions".into()),
            ],
        );
    }
    s.close("g");
    s.close("g");
}

fn draw_transition(s: &mut Svg, t: &Transition, pal: &Palette) {
    let pts = &t.points;
    let first = pts[0];
    let last = pts[pts.len() - 1];
    // A three-point transition is a curve through its recorded midpoint.
    let (d, from) = if pts.len() == 3 {
        let m = pts[1];
        let ctrl = Point::new(
            2.0 * m.x - (first.x + last.x) / 2.0,
            2.0 * m.y - (first.y + last.y) / 2.0,
        );
        (
            format!(
                "M{},{} Q{},{} {},{}",
                num(first.x),
                num(first.y),
                num(ctrl.x),
                num(ctrl.y),
                num(last.x),
                num(last.y)
            ),
            ctrl,
        )
    } else {
        let coords: Vec<(f64, f64)> = pts.iter().map(|p| (p.x, p.y)).collect();
        (format!("M{}", points_attr(&coords)), pts[pts.len() - 2])
    };
    s.open(
        "g",
        &[("class", "transition".into()), ("data-sid", t.id.clone())],
    );
    s.leaf("path", &[("d", d)]);

    let (mut dx, mut dy) = (last.x - from.x, last.y - from.y);
    let mut len = dx.hypot(dy);
    if len < 1e-6 {
        (dx, dy) = (last.x - first.x, last.y - first.y);
        len = dx.hypot(dy);
    }
    if len > 1e-6 {
        let (ux, uy) = (dx / len, dy / len);
        let base = (last.x - ux * 8.0, last.y - uy * 8.0);
        let (px, py) = (-uy * 3.5, ux * 3.5);
        let tri = [
            (last.x, last.y),
            (base.0 + px, base.1 + py),
            (base.0 - px, base.1 - py),
        ];
        s.leaf(
            "polygon",
            &[
                ("points", points_attr(&tri)),
                ("fill", pal.line.into()),
                ("stroke", "none".into()),
            ],
        );
    }
    if t.src.is_none() {
        s.leaf(
            "circle",
            &[
                ("cx", num(first.x)),
                ("cy", num(first.y)),
                ("r", "3".into()),
                ("fill", pal.line.into()),
                ("stroke", "none".into()),
                ("class", "default-transition".into()),
            ],
        );
    }
    if let Some((at, lines)) = transition_label(t) {
        let lines: Vec<&str> = lines.iter().map(|l| l.as_ref()).collect();
        s.text(
            at.x,
            at.y,
            &lines,
            TRANSITION_FONT,
            "start",
            false,
            &[("fill", pal.text.into()), ("stroke", "none".into())],
        );
    }
    s.close("g");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    fn state(id: &str, label: &str, r: Rect, subviewer: &str) -> State {
        State {
            id: id.into(),
            label: label.into(),
            position: r,
            parent: None,
            subviewer: Some(subviewer.into()),
            kind: StateKind::Or,
            script: None,
        }
    }

    fn chart() -> Chart {
        Chart {
            id: "9".into(),
            name: "Mode <logic>".into(),
            kind: ChartKind::StateChart,
            states: vec![
                state(
                    "1",
                    "On\nentry: x = 1 & y < 2;",
                    Rect::new(0.0, 0.0, 100.0, 60.0),
                    "9",
                ),
                state("2", "Off", Rect::new(200.0, 0.0, 300.0, 60.0), "9"),
                state("3", "Inner", Rect::new(10.0, 10.0, 50.0, 30.0), "2"),
            ],
            transitions: vec![
                Transition {
                    id: "4".into(),
                    label: String::new(),
                    src: None,
                    dst: Some("1".into()),
                    points: vec![Point::new(50.0, -30.0), Point::new(50.0, 0.0)],
                    label_position: None,
                    subviewer: Some("9".into()),
                },
                Transition {
                    id: "5".into(),
                    label: "[x > 1]".into(),
                    src: Some("1".into()),
                    dst: Some("2".into()),
                    points: vec![
                        Point::new(100.0, 30.0),
                        Point::new(150.0, 20.0),
                        Point::new(200.0, 30.0),
                    ],
                    label_position: Some(Point::new(130.0, 5.0)),
                    subviewer: Some("9".into()),
                },
            ],
            junctions: vec![unlinked_model::Junction {
                id: "6".into(),
                position: Rect::new(140.0, 80.0, 154.0, 94.0),
                kind: JunctionKind::History,
                subviewer: None,
            }],
            data: vec![],
            script: None,
            update_method: None,
            sample_time: None,
        }
    }

    #[test]
    fn notes_render_as_plain_text() {
        let mut c = chart();
        let mut note = state(
            "7",
            "<html><body><p>Remember &amp; check</p></body></html>",
            Rect::new(0.0, 100.0, 80.0, 120.0),
            "9",
        );
        note.kind = StateKind::Note;
        c.states.push(note);
        let svg = render_chart_svg(&c, &RenderOptions::default()).unwrap();
        assert!(svg.contains("class=\"note\""));
        assert!(svg.contains(">Remember &amp; check<"));
        assert!(!svg.contains("&lt;html"));
    }

    #[test]
    fn states_transitions_and_junctions_render() {
        let svg = render_chart_svg(&chart(), &RenderOptions::default()).unwrap();
        assert_eq!(svg.matches("class=\"state\"").count(), 2);
        assert!(svg.contains("data-subchart=\"true\""));
        assert!(svg.contains("font-weight=\"bold\""));
        assert!(svg.contains("entry: x = 1 &amp; y &lt; 2;"));
        assert!(svg.contains("class=\"default-transition\""));
        assert!(svg.contains("[x &gt; 1]"));
        assert!(svg.contains(" Q"));
        assert!(svg.contains(">H<"));
        assert!(!svg.contains("Inner"));
    }

    #[test]
    fn subchart_view_renders_its_contents() {
        let c = chart();
        let svg = render_chart_view_svg(&c, Some("2"), &RenderOptions::default()).unwrap();
        assert!(svg.contains("Inner"));
        assert!(!svg.contains("entry:"));
        assert_eq!(
            render_chart_view_svg(&c, Some("1"), &RenderOptions::default()),
            Err(RenderError::NoSuchView("1".into()))
        );
    }

    #[test]
    fn matlab_function_renders_code() {
        let mut c = chart();
        c.kind = ChartKind::MatlabFunction;
        c.script = Some("function y = f(u)\n\n\tif u < 0\n\t\ty = -u;\n\tend".into());
        let opts = RenderOptions {
            theme: Theme::Dark,
            ..Default::default()
        };
        let svg = render_chart_svg(&c, &opts).unwrap();
        assert!(svg.contains("MATLAB Function · Mode &lt;logic&gt;"));
        assert!(svg.contains("        y = -u;"));
        assert!(svg.contains("> </tspan>"), "blank lines keep their place");
        assert!(svg.contains("if u &lt; 0"));
        assert!(svg.contains("fill=\"#1a1b26\""));
        assert!(!svg.contains("class=\"state\""));
    }

    #[test]
    fn huge_scripts_are_truncated() {
        let mut c = chart();
        c.kind = ChartKind::MatlabFunction;
        c.script = Some("x = 1;\n".repeat(MAX_CODE_LINES + 10));
        let svg = render_chart_svg(&c, &RenderOptions::default()).unwrap();
        assert!(svg.contains("… 10 more lines"));
    }

    #[test]
    fn large_charts_render_in_linear_time() {
        let n = 50_000;
        let mut c = chart();
        c.states = (0..n)
            .map(|i| {
                let x = (i % 200) as f64 * 60.0;
                let y = (i / 200) as f64 * 40.0;
                // Half the states sit inside subcharts of the other half.
                let view = if i < n / 2 {
                    "9".to_string()
                } else {
                    (i - n / 2).to_string()
                };
                state(
                    &i.to_string(),
                    "S\nentry: x = 1;",
                    Rect::new(x, y, x + 50.0, y + 30.0),
                    &view,
                )
            })
            .collect();
        let start = std::time::Instant::now();
        let result = render_chart_svg(&c, &RenderOptions::default());
        assert!(matches!(result, Ok(_) | Err(RenderError::TooLarge)));
        assert!(render_chart_view_svg(&c, Some("7"), &RenderOptions::default()).is_ok());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn huge_labels_are_bounded() {
        let mut c = chart();
        c.states[0].label = format!("{}\n{}", "n".repeat(5_000_000), "a\n".repeat(1_000_000));
        c.transitions[1].label = "[x]\n".repeat(1_000_000);
        let start = std::time::Instant::now();
        let svg = render_chart_svg(&c, &RenderOptions::default()).unwrap();
        assert!(svg.len() < 200_000, "{} bytes", svg.len());
        assert!(start.elapsed() < std::time::Duration::from_secs(10));

        c.states = (0..5000)
            .map(|i| {
                let label = "y".repeat(MAX_LABEL_LINE_CHARS);
                let mut st = state(&i.to_string(), &label, Rect::new(0.0, 0.0, 9.0, 9.0), "9");
                st.label = format!("{label}\n").repeat(MAX_STATE_LINES);
                st
            })
            .collect();
        assert_eq!(
            render_chart_svg(&c, &RenderOptions::default()),
            Err(RenderError::TooLarge)
        );
    }

    #[test]
    fn huge_scripts_render_quickly() {
        let mut c = chart();
        c.kind = ChartKind::MatlabFunction;
        c.script = Some(format!("{}{}", "x".repeat(100_000), "\n".repeat(5_000_000)));
        let start = std::time::Instant::now();
        let svg = render_chart_svg(&c, &RenderOptions::default()).unwrap();
        assert!(svg.contains("… 4995000 more lines"));
        assert!(svg.len() < 1_000_000, "{} bytes", svg.len());
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn empty_chart_shows_note() {
        let mut c = chart();
        c.states.clear();
        c.transitions.clear();
        c.junctions.clear();
        c.kind = ChartKind::TruthTable;
        let svg = render_chart_svg(&c, &RenderOptions::default()).unwrap();
        assert!(svg.contains("Truth table"));
    }
}
