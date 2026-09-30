//! Interactive diagram viewer: pan/zoom, subsystem drill-down, block
//! inspector. The diagram itself is SVG produced by `unlinked-render` in
//! wasm; clicks are resolved through the `data-*` attributes it emits.
//! Opening a Stateflow block shows its chart (or MATLAB Function code)
//! instead of the generated plumbing inside it; path entries past the chart
//! block name subcharted states.

use gloo_events::{EventListener, EventListenerOptions};
use std::rc::Rc;
use unlinked_model::diff::{BlockChange, ModelDiff};
use unlinked_model::{Block, Chart, Model, System};
use unlinked_render::{render_chart_view_svg, render_svg, RenderOptions, Theme};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, MouseEvent, WheelEvent};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DiagramProps {
    pub model: Rc<Model>,
    /// Changes relative to an older version, highlighted on the diagram.
    #[prop_or_default]
    pub diff: Option<Rc<ModelDiff>>,
}

/// CSS outlining changed blocks in the system at `path`: added green,
/// modified orange, moved-only teal, and subsystems containing changes
/// dashed purple.
fn diff_css(diff: &ModelDiff, path: &[String], system: Option<&System>) -> String {
    let rule = |sid: &str, color: &str, dashed: bool| {
        format!(
            ".diagram g.block[data-sid=\"{}\"] > :is(rect, polygon, ellipse) {{ stroke: {color}; stroke-width: 3px;{} }}\n",
            css_string(sid),
            if dashed { " stroke-dasharray: 6 3;" } else { "" }
        )
    };
    let mut css = String::new();
    for b in diff.in_system(path) {
        match &b.change {
            BlockChange::Added => css.push_str(&rule(&b.id.0, "#9ece6a", false)),
            BlockChange::Modified(m) if m.layout_only() => {
                css.push_str(&rule(&b.id.0, "#7dcfff", false))
            }
            BlockChange::Modified(_) => css.push_str(&rule(&b.id.0, "#ff9e64", false)),
            BlockChange::Removed => {}
        }
    }
    if let Some(sys) = system {
        for b in sys.blocks.iter().filter(|b| b.subsystem.is_some()) {
            let mut sub = path.to_vec();
            sub.push(b.name.clone());
            if diff.touches(&sub) {
                css.push_str(&rule(&b.id.0, "#bb9af7", true));
            }
        }
    }
    css
}

#[derive(Clone, Copy, PartialEq)]
struct View {
    scale: f64,
    x: f64,
    y: f64,
}

enum ViewAction {
    Set(View),
    /// Multiply the scale by `factor`, keeping container point (cx, cy) fixed.
    Zoom {
        factor: f64,
        cx: f64,
        cy: f64,
    },
}

/// A reducer rather than plain state so long-lived listeners (the wheel
/// handler) always act on the current view instead of a captured snapshot.
impl Reducible for View {
    type Action = ViewAction;

    fn reduce(self: Rc<Self>, action: ViewAction) -> Rc<Self> {
        match action {
            ViewAction::Set(v) => Rc::new(v),
            ViewAction::Zoom { factor, cx, cy } => {
                let scale = (self.scale * factor).clamp(0.02, 20.0);
                let k = scale / self.scale;
                Rc::new(View {
                    scale,
                    x: cx - (cx - self.x) * k,
                    y: cy - (cy - self.y) * k,
                })
            }
        }
    }
}

/// Width and height from the `viewBox` of a rendered SVG.
fn svg_size(svg: &str) -> Option<(f64, f64)> {
    let vb = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
    let v: Vec<f64> = vb.split(' ').filter_map(|s| s.parse().ok()).collect();
    (v.len() == 4).then(|| (v[2], v[3]))
}

/// Fit the diagram inside the container, enlarging small diagrams at most
/// to 150%.
fn fit(container: &HtmlElement, size: (f64, f64)) -> View {
    let (cw, ch) = (
        container.client_width() as f64,
        container.client_height() as f64,
    );
    let scale = (cw / size.0).min(ch / size.1).clamp(0.02, 1.5);
    View {
        scale,
        x: (cw - size.0 * scale) / 2.0,
        y: (ch - size.1 * scale) / 2.0,
    }
}

fn closest(target: Option<web_sys::EventTarget>, selector: &str) -> Option<Element> {
    target?
        .dyn_into::<Element>()
        .ok()?
        .closest(selector)
        .ok()
        .flatten()
}

fn block_group(target: Option<web_sys::EventTarget>) -> Option<Element> {
    closest(target, "g.block")
}

/// The chart shown at `path`: the chart of the first block along it that has
/// one, with the remaining names selecting nested subcharts.
fn chart_at<'a, 'p>(model: &'a Model, path: &'p [&'p str]) -> Option<(&'a Chart, &'p [&'p str])> {
    (1..=path.len()).find_map(|i| model.chart_at(&path[..i]).map(|c| (c, &path[i..])))
}

/// Full name of state `sid` in the chart shown at `path`. States are
/// opened by id because the rendered `data-name` may be truncated.
fn subchart_name(model: &Model, path: &[&str], sid: &str) -> Option<String> {
    let (chart, _) = chart_at(model, path)?;
    chart.state(sid).map(|s| s.name().to_string())
}

fn render(model: &Model, path: &[&str], opts: &RenderOptions) -> Result<String, String> {
    match chart_at(model, path) {
        Some((chart, rest)) => {
            let view = chart
                .view_at(rest)
                .ok_or_else(|| format!("no subchart {:?} in {}", rest.join("/"), chart.name))?;
            render_chart_view_svg(chart, view, opts)
        }
        None => render_svg(model, path, opts),
    }
    .map_err(|e| e.to_string())
}

/// Escape text for use inside a double-quoted CSS string: quotes and
/// backslashes are escaped, control characters become hex escapes.
fn css_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\{:x} ", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[function_component(DiagramView)]
pub fn diagram_view(props: &DiagramProps) -> Html {
    let path = use_state(Vec::<String>::new);
    let selected = use_state(|| None::<String>);
    let theme = use_state(|| Theme::Dark);
    let view = use_reducer(|| View {
        scale: 1.0,
        x: 0.0,
        y: 0.0,
    });
    let drag = use_mut_ref(|| None::<(f64, f64, View, bool)>);
    let container = use_node_ref();

    let model = props.model.clone();
    let rendered = use_memo(
        (Rc::as_ptr(&props.model) as usize, (*path).clone(), *theme),
        move |(_, path, theme)| {
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            let opts = RenderOptions {
                theme: *theme,
                ..Default::default()
            };
            render(&model, &refs, &opts)
        },
    );

    // Fit whenever a new diagram level is shown.
    {
        let view = view.clone();
        let container = container.clone();
        let rendered = rendered.clone();
        use_effect_with(
            (Rc::as_ptr(&props.model) as usize, (*path).clone()),
            move |_| {
                if let (Some(el), Ok(svg)) = (container.cast::<HtmlElement>(), rendered.as_ref()) {
                    if let Some(size) = svg_size(svg) {
                        view.dispatch(ViewAction::Set(fit(&el, size)));
                    }
                }
            },
        );
    }

    // Wheel zoom around the cursor. Registered by hand so the listener is
    // not passive and can stop the page from scrolling.
    {
        let view = view.clone();
        let container = container.clone();
        use_effect_with((), move |_| {
            let listener = container.cast::<HtmlElement>().map(|el| {
                let target = el.clone();
                EventListener::new_with_options(
                    &el,
                    "wheel",
                    EventListenerOptions::enable_prevent_default(),
                    move |e| {
                        let e = e.dyn_ref::<WheelEvent>().unwrap();
                        e.prevent_default();
                        let rect = target.get_bounding_client_rect();
                        view.dispatch(ViewAction::Zoom {
                            factor: (-e.delta_y() * 0.0015).exp(),
                            cx: e.client_x() as f64 - rect.left(),
                            cy: e.client_y() as f64 - rect.top(),
                        });
                    },
                )
            });
            move || drop(listener)
        });
    }

    let onmousedown = {
        let drag = drag.clone();
        let view = view.clone();
        Callback::from(move |e: MouseEvent| {
            if e.button() == 0 {
                *drag.borrow_mut() = Some((e.client_x() as f64, e.client_y() as f64, *view, false));
            }
        })
    };
    let onmousemove = {
        let drag = drag.clone();
        let view = view.clone();
        Callback::from(move |e: MouseEvent| {
            let mut d = drag.borrow_mut();
            // A NaN anchor marks a drag that already ended (button released
            // or pointer left); only the click handler still reads it.
            if let Some((sx, sy, start, moved)) = d.as_mut().filter(|d| !d.0.is_nan()) {
                let (dx, dy) = (e.client_x() as f64 - *sx, e.client_y() as f64 - *sy);
                if dx.abs() + dy.abs() > 3.0 {
                    *moved = true;
                }
                if *moved {
                    view.dispatch(ViewAction::Set(View {
                        scale: start.scale,
                        x: start.x + dx,
                        y: start.y + dy,
                    }));
                }
            }
        })
    };
    let end_drag = {
        let drag = drag.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(d) = drag.borrow_mut().as_mut() {
                // Keep the "moved" flag for the click handler, drop the anchor.
                d.0 = f64::NAN;
            }
        })
    };
    let onclick = {
        let drag = drag.clone();
        let selected = selected.clone();
        Callback::from(move |e: MouseEvent| {
            let moved = drag.borrow_mut().take().is_some_and(|d| d.3);
            if moved {
                return;
            }
            selected.set(block_group(e.target()).and_then(|g| g.get_attribute("data-sid")));
        })
    };
    let ondblclick = {
        let path = path.clone();
        let selected = selected.clone();
        let model = props.model.clone();
        Callback::from(move |e: MouseEvent| {
            let name = if let Some(g) = closest(e.target(), "g.state[data-subchart]") {
                let refs: Vec<&str> = path.iter().map(String::as_str).collect();
                g.get_attribute("data-sid")
                    .and_then(|sid| subchart_name(&model, &refs, &sid))
            } else {
                closest(e.target(), "g.block[data-subsystem]")
                    .and_then(|g| g.get_attribute("data-name"))
            };
            if let Some(name) = name {
                let mut p = (*path).clone();
                p.push(name);
                path.set(p);
                selected.set(None);
            }
        })
    };

    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    let system = props.model.system_at(&refs);
    let selected_block = system.and_then(|s| {
        selected
            .as_ref()
            .and_then(|sid| s.blocks.iter().find(|b| &b.id.0 == sid))
    });
    let selected_chart = selected_block.and_then(|b| {
        let mut p = refs.clone();
        p.push(&b.name);
        props.model.chart_at(&p)
    });

    let open_path = {
        let path = path.clone();
        let selected = selected.clone();
        move |p: Vec<String>| {
            let path = path.clone();
            let selected = selected.clone();
            Callback::from(move |_: MouseEvent| {
                path.set(p.clone());
                selected.set(None);
            })
        }
    };

    let crumbs = {
        let mut items = vec![html! {
            <a class="crumb" onclick={open_path(vec![])}>{ &props.model.name }</a>
        }];
        for i in 0..path.len() {
            let p = path[..=i].to_vec();
            items.push(html! { <span class="crumb-sep">{ "/" }</span> });
            items.push(html! {
                <a class="crumb" onclick={open_path(p)}>{ path[i].replace('\n', " ") }</a>
            });
        }
        items
    };

    let toggle_theme = {
        let theme = theme.clone();
        Callback::from(move |_: MouseEvent| {
            theme.set(if *theme == Theme::Dark {
                Theme::Light
            } else {
                Theme::Dark
            })
        })
    };
    let fit_view = {
        let view = view.clone();
        let container = container.clone();
        let rendered = rendered.clone();
        Callback::from(move |_: MouseEvent| {
            if let (Some(el), Ok(svg)) = (container.cast::<HtmlElement>(), rendered.as_ref()) {
                if let Some(size) = svg_size(svg) {
                    view.dispatch(ViewAction::Set(fit(&el, size)));
                }
            }
        })
    };

    let highlight = selected
        .as_ref()
        .map(|sid| {
            format!(
                ".diagram g.block[data-sid=\"{}\"] > :is(rect, polygon, ellipse) {{ stroke: #ff9e64; stroke-width: 3px; }}",
                css_string(sid)
            )
        })
        .unwrap_or_default();
    let highlight = match &props.diff {
        Some(d) => diff_css(d, &path, system) + &highlight,
        None => highlight,
    };

    let canvas = match rendered.as_ref() {
        Ok(svg) => Html::from_html_unchecked(AttrValue::from(svg.clone())),
        Err(e) => html! { <div class="error">{ e }</div> },
    };
    let v = *view;
    let transform = format!(
        "transform: translate({}px, {}px) scale({}); transform-origin: 0 0;",
        v.x, v.y, v.scale
    );

    html! {
        <div class="viewer">
            <div class="toolbar">
                <nav class="crumbs">{ for crumbs }</nav>
                <span class="spacer" />
                <span class="zoom">{ format!("{:.0}%", v.scale * 100.0) }</span>
                <button onclick={fit_view}>{ "Fit" }</button>
                <button onclick={toggle_theme}>{ if *theme == Theme::Dark { "Light" } else { "Dark" } }</button>
            </div>
            <div class="viewer-body">
                <aside class="tree">
                    <SystemTree model={props.model.clone()} current={(*path).clone()} on_open={Callback::from({
                        let path = path.clone();
                        let selected = selected.clone();
                        move |p: Vec<String>| { path.set(p); selected.set(None); }
                    })} />
                </aside>
                <div class={classes!("diagram", (*theme == Theme::Light).then_some("light"))}
                    ref={container}
                    {onmousedown} {onmousemove} onmouseup={end_drag.clone()} onmouseleave={end_drag}
                    {onclick} {ondblclick}>
                    <style>{ highlight }</style>
                    <div class="canvas" style={transform}>{ canvas }</div>
                </div>
                if let Some(b) = selected_block {
                    <Inspector block={Rc::new(b.clone())} chart={selected_chart.map(|c| Rc::new(c.clone()))} on_open={Callback::from({
                        let path = path.clone();
                        let selected = selected.clone();
                        move |name: String| {
                            let mut p = (*path).clone();
                            p.push(name);
                            path.set(p);
                            selected.set(None);
                        }
                    })} />
                }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct TreeProps {
    model: Rc<Model>,
    current: Vec<String>,
    on_open: Callback<Vec<String>>,
}

#[function_component(SystemTree)]
fn system_tree(props: &TreeProps) -> Html {
    fn level(sys: &System, prefix: &[String], props: &TreeProps) -> Html {
        let subs: Vec<&Block> = sys
            .blocks
            .iter()
            .filter(|b| b.subsystem.is_some())
            .collect();
        if subs.is_empty() {
            return html! {};
        }
        html! {
            <ul>
                { for subs.into_iter().map(|b| {
                    let mut p = prefix.to_vec();
                    p.push(b.name.clone());
                    let active = p == props.current;
                    let on_open = props.on_open.clone();
                    let target = p.clone();
                    html! {
                        <li>
                            <a class={classes!(active.then_some("active"))}
                                onclick={Callback::from(move |_: MouseEvent| on_open.emit(target.clone()))}>
                                { b.name.replace('\n', " ") }
                            </a>
                            { level(b.subsystem.as_ref().unwrap(), &p, props) }
                        </li>
                    }
                }) }
            </ul>
        }
    }
    let on_open = props.on_open.clone();
    html! {
        <div class="system-tree">
            <a class={classes!(props.current.is_empty().then_some("active"))}
                onclick={Callback::from(move |_: MouseEvent| on_open.emit(vec![]))}>
                { &props.model.name }
            </a>
            { level(&props.model.root, &[], props) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct InspectorProps {
    block: Rc<Block>,
    /// Stateflow chart implementing the block.
    chart: Option<Rc<Chart>>,
    on_open: Callback<String>,
}

#[function_component(Inspector)]
fn inspector(props: &InspectorProps) -> Html {
    let b = &props.block;
    let open = b.subsystem.is_some().then(|| {
        let on_open = props.on_open.clone();
        let name = b.name.clone();
        let label = if props.chart.is_some() {
            "Open chart"
        } else {
            "Open subsystem"
        };
        html! { <button onclick={Callback::from(move |_: MouseEvent| on_open.emit(name.clone()))}>{ label }</button> }
    });
    let kind = b.stateflow_type().unwrap_or_else(|| b.display_type());
    let script = props.chart.as_ref().and_then(|c| c.script.clone());
    html! {
        <aside class="inspector">
            <h3>{ b.name.replace('\n', " ") }</h3>
            <div class="muted">{ format!("{kind} · SID {}", b.id) }</div>
            { for open }
            if let Some(script) = script {
                <h4>{ "MATLAB code" }</h4>
                <pre class="script"><code>{ script }</code></pre>
            }
            if let Some(src) = &b.library_source {
                <div class="muted">{ format!("Library: {}", src.replace('\n', " ")) }</div>
            }
            if let Some(mask) = &b.mask {
                <h4>{ "Mask parameters" }</h4>
                <table>
                    { for mask.parameters.iter().map(|p| html! {
                        <tr><td title={p.prompt.clone().unwrap_or_default()}>{ &p.name }</td><td><code>{ &p.value }</code></td></tr>
                    }) }
                </table>
            }
            <h4>{ "Parameters" }</h4>
            <table>
                { for b.parameters.iter().filter(|(k, _)| k.as_str() != "ZOrder").map(|(k, v)| html! {
                    <tr><td>{ k }</td><td><code>{ v }</code></td></tr>
                }) }
            </table>
        </aside>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlinked_model::{ChartKind, Rect, SimConfig, SourceFormat, State, StateKind};

    fn state(id: &str, label: &str, subviewer: &str) -> State {
        State {
            id: id.into(),
            label: label.into(),
            position: Rect::new(0.0, 0.0, 50.0, 30.0),
            parent: None,
            subviewer: Some(subviewer.into()),
            kind: StateKind::Or,
            script: None,
        }
    }

    #[test]
    fn long_subchart_names_stay_navigable() {
        let long = "L".repeat(300);
        let model = Model {
            name: "m".into(),
            source: SourceFormat::Slx,
            simulink_version: None,
            config: SimConfig::default(),
            root: System::default(),
            workspace: Default::default(),
            charts: vec![Chart {
                id: "9".into(),
                name: "Sub".into(),
                kind: ChartKind::StateChart,
                states: vec![state("1", &long, "9"), state("2", "Inner", "1")],
                transitions: vec![],
                junctions: vec![],
                data: vec![],
                script: None,
                update_method: None,
                sample_time: None,
            }],
        };
        let opts = RenderOptions::default();
        let top = render(&model, &["Sub"], &opts).unwrap();
        assert!(top.contains("data-sid=\"1\""));
        assert!(!top.contains(&format!("data-name=\"{long}\"")));

        let name = subchart_name(&model, &["Sub"], "1").unwrap();
        assert_eq!(name, long);
        let inner = render(&model, &["Sub", &name], &opts).unwrap();
        assert!(inner.contains("Inner"));
    }
}
