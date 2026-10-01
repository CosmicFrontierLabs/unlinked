//! Interactive diagram viewer: pan/zoom, subsystem drill-down, block
//! inspector. The diagram itself is SVG produced by `unlinked-render` in
//! wasm; clicks are resolved through the `data-*` attributes it emits.
//! Opening a Stateflow block shows its chart (or MATLAB Function code)
//! instead of the generated plumbing inside it; path entries past the chart
//! block name subcharted states.

use gloo_events::{EventListener, EventListenerOptions};
use std::rc::Rc;
use unlinked_model::diff::{BlockChange, ModelDiff};
use unlinked_model::edit::{system_ids, touches, DisconnectPolicy, Edit, SystemRef};
use unlinked_model::scope::ScopeConfig;
use unlinked_model::{catalog, geometry};
use unlinked_model::{
    Block, BlockId, Chart, Endpoint, Line, Model, Point, PortKind, PortRef, Rect, System,
};
use unlinked_render::{render_chart_view_svg, render_svg, RenderOptions, Theme};
use wasm_bindgen::JsCast;
use web_sys::{
    DragEvent, Element, HtmlElement, HtmlInputElement, KeyboardEvent, MouseEvent, WheelEvent,
};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DiagramProps {
    pub model: Rc<Model>,
    /// Changes relative to an older version, highlighted on the diagram.
    #[prop_or_default]
    pub diff: Option<Rc<ModelDiff>>,
    /// Enables editing: blocks can be dragged, renamed, re-parameterized and
    /// deleted, and each change is reported here.
    #[prop_or_default]
    /// Each call carries one user action, which undo treats as a unit.
    pub on_edit: Option<Callback<Vec<Edit>>>,
    /// Why an editing action could not be turned into edits.
    #[prop_or_default]
    pub on_error: Option<Callback<String>>,
    /// The view re-fits when this changes; by default whenever the model
    /// changes. Editors pass a stable key so edits keep the current view.
    #[prop_or_default]
    pub fit_key: Option<AttrValue>,
}

/// Grid block positions snap to when dragged.
const SNAP: f64 = 5.0;

/// Pointer interaction in progress on the diagram.
enum Drag {
    Pan {
        sx: f64,
        sy: f64,
        start: View,
        moved: bool,
    },
    /// Moving the selected blocks: each one's rendered group, id and outline.
    Blocks {
        sx: f64,
        sy: f64,
        blocks: Vec<(Element, String, Rect)>,
        offset: (f64, f64),
        moved: bool,
    },
    /// Drawing a selection box from `start`; `extend` adds to the selection.
    Select { start: Point, extend: bool },
    /// Dragging segment `segment` of the drawn wire `points` into `dst`
    /// across itself, from diagram point `start`; see
    /// [`geometry::drag_segment`].
    Segment {
        dst: Endpoint,
        points: Vec<Point>,
        fixed: usize,
        segment: usize,
        start: Point,
        route: Option<Vec<Point>>,
    },
    /// Dragging a new connection out of port `from`.
    Wire { from: Endpoint },
    /// Dragging a corner of block `id`, whose outline was `rect`; `right`
    /// and `bottom` say which corner.
    Resize {
        sx: f64,
        sy: f64,
        id: BlockId,
        rect: Rect,
        right: bool,
        bottom: bool,
    },
    /// Released; kept so the following click knows whether it was a drag.
    Ended { moved: bool },
}

/// `dataTransfer` type carrying a palette block's catalog key.
const PALETTE_DRAG: &str = "application/x-unlinked-block";

/// Whether a line may start at a port of this kind.
fn is_source(kind: PortKind) -> bool {
    matches!(kind, PortKind::Out | PortKind::State)
}

/// The endpoint an element's `data-{prefix}sid/kind/index` attributes name.
fn endpoint_of(el: &Element, prefix: &str) -> Option<Endpoint> {
    let attr = |name: &str| el.get_attribute(&format!("data-{prefix}{name}"));
    Some(Endpoint {
        block: BlockId(attr("sid")?),
        port: PortRef {
            kind: PortKind::from_token(&attr("kind")?)?,
            index: attr("index")?.parse().ok()?,
        },
    })
}

/// The `viewBox` of a rendered SVG: origin and size in diagram units.
fn view_box(svg: &str) -> Option<[f64; 4]> {
    let vb = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
    let v: Vec<f64> = vb.split(' ').filter_map(|s| s.parse().ok()).collect();
    v.try_into().ok()
}

/// Diagram coordinates of a pointer event over the container.
fn to_diagram(e: &MouseEvent, container: &NodeRef, view: &View, svg: &str) -> Option<Point> {
    let rect = container.cast::<HtmlElement>()?.get_bounding_client_rect();
    let [vx, vy, ..] = view_box(svg)?;
    Some(Point::new(
        (e.client_x() as f64 - rect.left() - view.x) / view.scale + vx,
        (e.client_y() as f64 - rect.top() - view.y) / view.scale + vy,
    ))
}

/// The points of a rendered `polyline`.
fn polyline_points(el: &Element) -> Vec<Point> {
    el.get_attribute("points")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pair| {
            let (x, y) = pair.split_once(',')?;
            Some(Point::new(x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}

/// Index of the segment of `points` nearest `at`, among those from `first`.
fn nearest_segment(points: &[Point], first: usize, at: Point) -> Option<usize> {
    let distance = |a: Point, b: Point| {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx * dx + dy * dy;
        let t = if len > 0.0 {
            (((at.x - a.x) * dx + (at.y - a.y) * dy) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (a.x + t * dx - at.x, a.y + t * dy - at.y);
        px * px + py * py
    };
    (first..points.len().saturating_sub(1)).min_by(|&i, &j| {
        distance(points[i], points[i + 1]).total_cmp(&distance(points[j], points[j + 1]))
    })
}

/// Blocks copied with Ctrl+C, with the model as it was then; see
/// [`unlinked_model::clipboard::paste`].
struct Clipboard {
    model: Rc<Model>,
    system: SystemRef,
    ids: Vec<BlockId>,
    /// Pastes so far; each lands further down and right.
    pastes: u32,
}

/// Smallest block side a resize leaves.
const MIN_SIDE: f64 = 10.0;

/// `rect` with the corner selected by `right`/`bottom` moved by (dx, dy)
/// diagram units, snapped, and kept at least [`MIN_SIDE`] across.
fn resized(rect: Rect, right: bool, bottom: bool, dx: f64, dy: f64) -> Rect {
    let snap = |v: f64| (v / SNAP).round() * SNAP;
    let mut r = rect;
    if right {
        r.right = snap(rect.right + dx).max(rect.left + MIN_SIDE);
    } else {
        r.left = snap(rect.left + dx).min(rect.right - MIN_SIDE);
    }
    if bottom {
        r.bottom = snap(rect.bottom + dy).max(rect.top + MIN_SIDE);
    } else {
        r.top = snap(rect.top + dy).min(rect.bottom - MIN_SIDE);
    }
    r
}

/// `base`, or `base` with the smallest number appended that no block in
/// `sys` is named, as Simulink names copies.
fn unique_name(sys: &System, base: &str) -> String {
    let names: std::collections::HashSet<&str> =
        sys.blocks.iter().map(|b| b.name.as_str()).collect();
    let taken = |n: &str| names.contains(n);
    if !taken(base) {
        return base.to_string();
    }
    (1..)
        .map(|i| format!("{base}{i}"))
        .find(|n| !taken(n))
        .expect("some suffix is free")
}

/// The edit adding palette block `type_key` centered at `at`.
fn add_block_at(model: &Model, system: &SystemRef, type_key: &str, at: Point) -> Option<Edit> {
    let descriptor = catalog::find(type_key)?;
    let names = unlinked_model::edit::system_names(model, system)?;
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let sys = model.system_at(&refs)?;
    let snap = |v: f64| (v / SNAP).round() * SNAP;
    let [w, h] = descriptor.default_size;
    let (left, top) = (snap(at.x - w / 2.0), snap(at.y - h / 2.0));
    Some(Edit::AddBlock {
        system: system.clone(),
        id: BlockId(unlinked_model::edit::next_sid(model)?.to_string()),
        block_type: type_key.into(),
        name: unique_name(sys, descriptor.label),
        position: Rect::new(left, top, left + w, top + h),
    })
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
    view_box(svg).map(|[_, _, w, h]| (w, h))
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

/// The edits deleting blocks `ids`, or `None` if the user declines. Blocks
/// with lines attached are only deleted, lines included, after confirming;
/// otherwise the edits refuse to touch lines at all.
fn confirm_delete(system: &SystemRef, ids: &[BlockId], lines: &[Line]) -> Option<Vec<Edit>> {
    let attached = lines
        .iter()
        .filter(|l| ids.iter().any(|id| touches(l, id)))
        .count();
    let disconnect = if attached == 0 {
        DisconnectPolicy::Reject
    } else {
        let what = if ids.len() == 1 {
            "this block".to_string()
        } else {
            format!("these {} blocks", ids.len())
        };
        let message = format!(
            "Delete {what} and the {attached} line{} connected to {}?",
            if attached == 1 { "" } else { "s" },
            if ids.len() == 1 { "it" } else { "them" }
        );
        if !gloo_utils::window()
            .confirm_with_message(&message)
            .unwrap_or(false)
        {
            return None;
        }
        DisconnectPolicy::Disconnect
    };
    Some(
        ids.iter()
            .map(|id| Edit::DeleteBlock {
                system: system.clone(),
                id: id.clone(),
                disconnect,
            })
            .collect(),
    )
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
    // Selected blocks, by SID; the inspector shows a lone selection.
    let selected = use_state(Vec::<String>::new);
    // The selection box being drawn, corner to corner.
    let select_box = use_state(|| None::<(Point, Point)>);
    // A selected connection, by the input it drives.
    let selected_wire = use_state(|| None::<Endpoint>);
    // A connection being dragged: from its first port to the pointer.
    let wire_preview = use_state(|| None::<(Point, Point)>);
    // The outline of a block being resized.
    let resize_preview = use_state(|| None::<Rect>);
    // A wire being rerouted, as it would be drawn.
    let route_preview = use_state(|| None::<Vec<Point>>);
    let theme = use_state(|| Theme::Dark);
    let view = use_reducer(|| View {
        scale: 1.0,
        x: 0.0,
        y: 0.0,
    });
    let drag = use_mut_ref(|| None::<Drag>);
    // Blocks copied with Ctrl+C.
    let clipboard = use_mut_ref(|| None::<Clipboard>);
    let container = use_node_ref();
    let fit_key = props
        .fit_key
        .clone()
        .unwrap_or_else(|| AttrValue::from(format!("{:p}", Rc::as_ptr(&props.model))));

    let model = props.model.clone();
    let editable = props.on_edit.is_some();
    // Most actions are a single edit.
    let on_edit: Option<Callback<Edit>> = props
        .on_edit
        .as_ref()
        .map(|cb| cb.reform(|edit| vec![edit]));
    let rendered = use_memo(
        (
            Rc::as_ptr(&props.model) as usize,
            (*path).clone(),
            *theme,
            editable,
        ),
        move |(_, path, theme, editable)| {
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            let opts = RenderOptions {
                theme: *theme,
                hit_targets: *editable,
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
        use_effect_with((fit_key.clone(), (*path).clone()), move |_| {
            if let (Some(el), Ok(svg)) = (container.cast::<HtmlElement>(), rendered.as_ref()) {
                if let Some(size) = svg_size(svg) {
                    view.dispatch(ViewAction::Set(fit(&el, size)));
                }
            }
        });
    }

    // A selected connection belongs to the level it was picked on.
    {
        let selected_wire = selected_wire.clone();
        use_effect_with((*path).clone(), move |_| selected_wire.set(None));
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

    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    let system = props.model.system_at(&refs);
    // Edits address the shown system by block IDs, which survive renames.
    let system_ref: SystemRef = system_ids(&props.model, &path).unwrap_or_default();

    let onmousedown = {
        let (drag, view, on_edit) = (drag.clone(), view.clone(), on_edit.clone());
        let blocks: Vec<(String, Rect)> = system
            .map(|s| {
                s.blocks
                    .iter()
                    .map(|b| (b.id.0.clone(), b.position))
                    .collect()
            })
            .unwrap_or_default();
        let (container, rendered, wire_preview, selected, select_box) = (
            container.clone(),
            rendered.clone(),
            wire_preview.clone(),
            selected.clone(),
            select_box.clone(),
        );
        Callback::from(move |e: MouseEvent| {
            let (sx, sy) = (e.client_x() as f64, e.client_y() as f64);
            // The middle button always pans; while editing, the left button
            // on empty canvas draws a selection box instead.
            if e.button() == 1 {
                e.prevent_default();
                *drag.borrow_mut() = Some(Drag::Pan {
                    sx,
                    sy,
                    start: *view,
                    moved: false,
                });
                return;
            }
            if e.button() != 0 {
                return;
            }
            let port = on_edit
                .as_ref()
                .and_then(|_| endpoint_of(&closest(e.target(), "circle.port-hit")?, ""));
            if let Some(from) = port {
                let start = (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg));
                wire_preview.set(start.map(|p| (p, p)));
                *drag.borrow_mut() = Some(Drag::Wire { from });
                return;
            }
            let handle = closest(e.target(), "rect.resize-handle").and_then(|h| {
                let id = h.get_attribute("data-sid")?;
                let corner = h.get_attribute("data-corner")?;
                let rect = blocks.iter().find(|(b, _)| *b == id)?.1;
                Some((BlockId(id), rect, corner))
            });
            if let Some((id, rect, corner)) = handle {
                *drag.borrow_mut() = Some(Drag::Resize {
                    sx,
                    sy,
                    id,
                    rect,
                    right: corner.ends_with('e'),
                    bottom: corner.starts_with('s'),
                });
                return;
            }
            // Pressing on a wire may start dragging its nearest segment; a
            // press without movement still selects the wire on click.
            let segment = on_edit.as_ref().and_then(|_| {
                let hit = closest(e.target(), "polyline.wire-hit")?;
                let dst = endpoint_of(&hit, "dst-")?;
                let fixed: usize = hit.get_attribute("data-fixed")?.parse().ok()?;
                let points = polyline_points(&hit);
                let start = (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg))?;
                let segment = nearest_segment(&points, fixed.saturating_sub(1), start)?;
                Some(Drag::Segment {
                    dst,
                    points,
                    fixed,
                    segment,
                    start,
                    route: None,
                })
            });
            if let Some(segment) = segment {
                *drag.borrow_mut() = Some(segment);
                return;
            }
            let grabbed = on_edit
                .as_ref()
                .and_then(|_| block_group(e.target())?.get_attribute("data-sid"));
            if let Some(id) = grabbed {
                // Grabbing a selected block moves the whole selection.
                let ids = if selected.contains(&id) {
                    (*selected).clone()
                } else {
                    vec![id]
                };
                let moving = ids
                    .iter()
                    .filter_map(|id| {
                        let rect = blocks.iter().find(|(b, _)| b == id)?.1;
                        // Within this diagram: other viewers may show the
                        // same SIDs.
                        let selector = format!("g.block[data-sid=\"{}\"]", css_string(id));
                        let group = container
                            .cast::<Element>()?
                            .query_selector(&selector)
                            .ok()??;
                        Some((group, id.clone(), rect))
                    })
                    .collect();
                *drag.borrow_mut() = Some(Drag::Blocks {
                    sx,
                    sy,
                    blocks: moving,
                    offset: (0.0, 0.0),
                    moved: false,
                });
                return;
            }
            let start = on_edit.as_ref().and_then(|_| {
                (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg))
            });
            *drag.borrow_mut() = Some(match start {
                Some(start) => {
                    select_box.set(Some((start, start)));
                    Drag::Select {
                        start,
                        extend: e.shift_key(),
                    }
                }
                None => Drag::Pan {
                    sx,
                    sy,
                    start: *view,
                    moved: false,
                },
            });
        })
    };
    let onmousemove = {
        let (drag, view) = (drag.clone(), view.clone());
        let (container, rendered, wire_preview, resize_preview, select_box, route_preview) = (
            container.clone(),
            rendered.clone(),
            wire_preview.clone(),
            resize_preview.clone(),
            select_box.clone(),
            route_preview.clone(),
        );
        Callback::from(move |e: MouseEvent| {
            let (cx, cy) = (e.client_x() as f64, e.client_y() as f64);
            match drag.borrow_mut().as_mut() {
                Some(Drag::Resize {
                    sx,
                    sy,
                    rect,
                    right,
                    bottom,
                    ..
                }) => {
                    let (dx, dy) = ((cx - *sx) / view.scale, (cy - *sy) / view.scale);
                    resize_preview.set(Some(resized(*rect, *right, *bottom, dx, dy)));
                }
                Some(Drag::Wire { .. }) => {
                    let to = (*rendered)
                        .as_ref()
                        .ok()
                        .and_then(|svg| to_diagram(&e, &container, &view, svg));
                    if let (Some((from, _)), Some(to)) = (*wire_preview, to) {
                        wire_preview.set(Some((from, to)));
                    }
                }
                Some(Drag::Pan {
                    sx,
                    sy,
                    start,
                    moved,
                }) => {
                    let (dx, dy) = (cx - *sx, cy - *sy);
                    *moved |= dx.abs() + dy.abs() > 3.0;
                    if *moved {
                        view.dispatch(ViewAction::Set(View {
                            scale: start.scale,
                            x: start.x + dx,
                            y: start.y + dy,
                        }));
                    }
                }
                Some(Drag::Blocks {
                    sx,
                    sy,
                    blocks,
                    offset,
                    moved,
                }) => {
                    let (dx, dy) = (cx - *sx, cy - *sy);
                    *moved |= dx.abs() + dy.abs() > 3.0;
                    if *moved {
                        // Snap in diagram units; preview by translating the
                        // blocks' groups until the edits are applied.
                        let snap = |v: f64| (v / view.scale / SNAP).round() * SNAP;
                        *offset = (snap(dx), snap(dy));
                        for (group, ..) in blocks.iter() {
                            let _ = group.set_attribute(
                                "transform",
                                &format!("translate({} {})", offset.0, offset.1),
                            );
                        }
                    }
                }
                Some(Drag::Select { start, .. }) => {
                    let to = (*rendered)
                        .as_ref()
                        .ok()
                        .and_then(|svg| to_diagram(&e, &container, &view, svg));
                    if let Some(to) = to {
                        select_box.set(Some((*start, to)));
                    }
                }
                Some(Drag::Segment {
                    points,
                    fixed,
                    segment,
                    start,
                    route,
                    ..
                }) => {
                    let Some(to) = (*rendered)
                        .as_ref()
                        .ok()
                        .and_then(|svg| to_diagram(&e, &container, &view, svg))
                    else {
                        return;
                    };
                    let (a, b) = (points[*segment], points[*segment + 1]);
                    let across = if (b.x - a.x).abs() >= (b.y - a.y).abs() {
                        to.y - start.y
                    } else {
                        to.x - start.x
                    };
                    let delta = (across / SNAP).round() * SNAP;
                    *route = (delta != 0.0)
                        .then(|| geometry::drag_segment(points, *fixed, *segment, delta))
                        .flatten();
                    // Preview as drawn: fixed start, new vertices, port end.
                    route_preview.set(route.as_ref().map(|r| {
                        let mut drawn = points[..*fixed].to_vec();
                        drawn.extend(r);
                        drawn.extend(&points[points.len() - 2..]);
                        drawn
                    }));
                }
                _ => {}
            }
        })
    };
    let end_drag = {
        let (drag, on_edit, on_edits, system_ref) = (
            drag.clone(),
            on_edit.clone(),
            props.on_edit.clone(),
            system_ref.clone(),
        );
        let (wire_preview, resize_preview, select_box, route_preview) = (
            wire_preview.clone(),
            resize_preview.clone(),
            select_box.clone(),
            route_preview.clone(),
        );
        let (selected, selected_wire, view) =
            (selected.clone(), selected_wire.clone(), view.clone());
        let blocks: Vec<(String, Rect)> = system
            .map(|s| {
                s.blocks
                    .iter()
                    .map(|b| (b.id.0.clone(), b.position))
                    .collect()
            })
            .unwrap_or_default();
        Callback::from(move |e: MouseEvent| {
            let mut d = drag.borrow_mut();
            let moved = match d.take() {
                Some(Drag::Resize { id, rect, .. }) => {
                    let to = *resize_preview;
                    resize_preview.set(None);
                    if let (Some(position), Some(on_edit)) = (to.filter(|r| *r != rect), &on_edit) {
                        on_edit.emit(Edit::MoveBlock {
                            system: system_ref.clone(),
                            id,
                            position,
                        });
                    }
                    true
                }
                // Dropped on a port of the opposite direction: connect,
                // whichever end the drag started from.
                Some(Drag::Wire { from }) => {
                    wire_preview.set(None);
                    let to =
                        closest(e.target(), "circle.port-hit").and_then(|c| endpoint_of(&c, ""));
                    let pair = match to {
                        Some(to) if is_source(from.port.kind) && !is_source(to.port.kind) => {
                            Some((from, to))
                        }
                        Some(to) if !is_source(from.port.kind) && is_source(to.port.kind) => {
                            Some((to, from))
                        }
                        _ => None,
                    };
                    if let (Some((src, dst)), Some(on_edit)) = (pair, &on_edit) {
                        on_edit.emit(Edit::Connect {
                            system: system_ref.clone(),
                            src,
                            dst,
                        });
                    }
                    true
                }
                Some(Drag::Pan { moved, .. }) => moved,
                Some(Drag::Segment { dst, route, .. }) => {
                    route_preview.set(None);
                    let moved = route.is_some();
                    if let (Some(points), Some(on_edit)) = (route, &on_edit) {
                        on_edit.emit(Edit::SetRoute {
                            system: system_ref.clone(),
                            dst,
                            points,
                        });
                    }
                    moved
                }
                Some(Drag::Blocks {
                    blocks,
                    offset,
                    moved,
                    ..
                }) => {
                    if let (true, Some(on_edits)) = (moved && offset != (0.0, 0.0), &on_edits) {
                        let (dx, dy) = offset;
                        on_edits.emit(
                            blocks
                                .into_iter()
                                .map(|(_, id, r)| Edit::MoveBlock {
                                    system: system_ref.clone(),
                                    id: BlockId(id),
                                    position: Rect::new(
                                        r.left + dx,
                                        r.top + dy,
                                        r.right + dx,
                                        r.bottom + dy,
                                    ),
                                })
                                .collect(),
                        );
                    }
                    moved
                }
                // A box selects every block it touches; a click without
                // dragging is left to the click handler.
                Some(Drag::Select { start, extend }) => {
                    let end = select_box.map_or(start, |(_, end)| end);
                    select_box.set(None);
                    let area = Rect::new(
                        start.x.min(end.x),
                        start.y.min(end.y),
                        start.x.max(end.x),
                        start.y.max(end.y),
                    );
                    let dragged = area.width() + area.height() > 3.0 / view.scale;
                    if dragged {
                        let mut ids = if extend {
                            (*selected).clone()
                        } else {
                            Vec::new()
                        };
                        for (id, r) in &blocks {
                            let touches = r.left <= area.right
                                && r.right >= area.left
                                && r.top <= area.bottom
                                && r.bottom >= area.top;
                            if touches && !ids.contains(id) {
                                ids.push(id.clone());
                            }
                        }
                        selected.set(ids);
                        selected_wire.set(None);
                    }
                    dragged
                }
                Some(Drag::Ended { moved }) => moved,
                None => return,
            };
            *d = Some(Drag::Ended { moved });
        })
    };
    let onclick = {
        let drag = drag.clone();
        let (selected, selected_wire) = (selected.clone(), selected_wire.clone());
        Callback::from(move |e: MouseEvent| {
            let moved = matches!(drag.borrow_mut().take(), Some(Drag::Ended { moved: true }));
            if moved {
                return;
            }
            let wire =
                closest(e.target(), "polyline.wire-hit").and_then(|w| endpoint_of(&w, "dst-"));
            let block = block_group(e.target()).and_then(|g| g.get_attribute("data-sid"));
            selected.set(match (wire.is_some(), block) {
                (true, _) => Vec::new(),
                // Shift-click adds or removes a block.
                (false, Some(id)) if e.shift_key() => {
                    let mut ids = (*selected).clone();
                    match ids.iter().position(|s| *s == id) {
                        Some(i) => {
                            ids.remove(i);
                        }
                        None => ids.push(id),
                    }
                    ids
                }
                (false, Some(id)) => vec![id],
                (false, None) => Vec::new(),
            });
            selected_wire.set(wire);
        })
    };
    let onkeydown = {
        let (selected, selected_wire, on_edit, on_edits, system_ref) = (
            selected.clone(),
            selected_wire.clone(),
            on_edit.clone(),
            props.on_edit.clone(),
            system_ref.clone(),
        );
        let lines: Vec<Line> = system.map(|s| s.lines.clone()).unwrap_or_default();
        let blocks: Vec<Block> = system.map(|s| s.blocks.clone()).unwrap_or_default();
        let (clipboard, model, on_error) = (
            clipboard.clone(),
            props.model.clone(),
            props.on_error.clone(),
        );
        Callback::from(move |e: KeyboardEvent| {
            let (Some(on_edit), Some(on_edits)) = (&on_edit, &on_edits) else {
                return;
            };
            let command = e.ctrl_key() || e.meta_key();
            let key = e.key().to_ascii_lowercase();
            // Copy (Ctrl+C) the selected blocks; paste (Ctrl+V) duplicates
            // them with their internal lines, offset from the originals.
            if command && key == "c" && !selected.is_empty() {
                e.prevent_default();
                *clipboard.borrow_mut() = Some(Clipboard {
                    model: model.clone(),
                    system: system_ref.clone(),
                    ids: selected.iter().cloned().map(BlockId).collect(),
                    pastes: 0,
                });
                return;
            }
            if command && key == "v" {
                let mut clip = clipboard.borrow_mut();
                let Some(clip) = clip.as_mut() else {
                    return;
                };
                e.prevent_default();
                let pasted = if clip.system == system_ref {
                    unlinked_model::clipboard::paste(
                        &clip.model,
                        &clip.system,
                        &clip.ids,
                        clip.pastes,
                        &model,
                    )
                    .map_err(|e| e.to_string())
                } else {
                    Err("blocks paste only into the system they were copied from".into())
                };
                match pasted {
                    Ok(group) => {
                        clip.pastes = clip.pastes.saturating_add(1);
                        let added = group
                            .iter()
                            .filter_map(|edit| match edit {
                                Edit::AddBlock { id, .. } => Some(id.0.clone()),
                                _ => None,
                            })
                            .collect();
                        on_edits.emit(group);
                        selected.set(added);
                        selected_wire.set(None);
                    }
                    Err(message) => {
                        if let Some(on_error) = &on_error {
                            on_error.emit(format!("Cannot paste: {message}"));
                        }
                    }
                }
                return;
            }
            // Rotate (Ctrl+R) and flip (Ctrl+I), as in Simulink.
            if command && (key == "r" || key == "i") {
                let chosen: Vec<&Block> = blocks
                    .iter()
                    .filter(|b| selected.contains(&b.id.0))
                    .collect();
                if chosen.is_empty() {
                    return;
                }
                e.prevent_default();
                // Each selected block turns or flips in place, as one action.
                let mut group = Vec::new();
                for b in chosen {
                    let (orientation, mirrored) = if key == "r" {
                        geometry::rotated(b.orientation, b.mirrored)
                    } else {
                        geometry::flipped(b.orientation, b.mirrored)
                    };
                    if key == "r" {
                        group.push(Edit::MoveBlock {
                            system: system_ref.clone(),
                            id: b.id.clone(),
                            position: geometry::quarter_turn(b.position),
                        });
                    }
                    group.push(Edit::SetOrientation {
                        system: system_ref.clone(),
                        id: b.id.clone(),
                        orientation,
                        mirrored,
                    });
                }
                on_edits.emit(group);
                return;
            }
            // Select every block (Ctrl+A).
            if command && key == "a" {
                e.prevent_default();
                selected.set(blocks.iter().map(|b| b.id.0.clone()).collect());
                selected_wire.set(None);
                return;
            }
            if !matches!(e.key().as_str(), "Delete" | "Backspace") {
                return;
            }
            if let Some(dst) = (*selected_wire).clone() {
                e.prevent_default();
                on_edit.emit(Edit::Disconnect {
                    system: system_ref.clone(),
                    dst,
                });
                selected_wire.set(None);
            } else if !selected.is_empty() {
                e.prevent_default();
                let ids: Vec<BlockId> = selected.iter().cloned().map(BlockId).collect();
                if let Some(group) = confirm_delete(&system_ref, &ids, &lines) {
                    on_edits.emit(group);
                    selected.set(Vec::new());
                }
            }
        })
    };
    // Palette blocks are dragged in with HTML drag and drop.
    let ondragover = {
        let editable = props.on_edit.is_some();
        Callback::from(move |e: DragEvent| {
            let palette = e
                .data_transfer()
                .is_some_and(|d| d.types().includes(&PALETTE_DRAG.into(), 0));
            if editable && palette {
                e.prevent_default();
            }
        })
    };
    let ondrop = {
        let (on_edit, system_ref, model) =
            (on_edit.clone(), system_ref.clone(), props.model.clone());
        let (container, rendered, view) = (container.clone(), rendered.clone(), view.clone());
        Callback::from(move |e: DragEvent| {
            let Some(on_edit) = &on_edit else {
                return;
            };
            let Some(type_key) = e
                .data_transfer()
                .and_then(|d| d.get_data(PALETTE_DRAG).ok())
            else {
                return;
            };
            e.prevent_default();
            let at = (*rendered)
                .as_ref()
                .ok()
                .and_then(|svg| to_diagram(&e, &container, &view, svg));
            if let Some(edit) = at.and_then(|at| add_block_at(&model, &system_ref, &type_key, at)) {
                on_edit.emit(edit);
            }
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
                selected.set(Vec::new());
            }
        })
    };

    // The inspector and resize handles follow a lone selected block.
    let selected_block = system.and_then(|s| match selected.as_slice() {
        [sid] => s.blocks.iter().find(|b| &b.id.0 == sid),
        _ => None,
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
                selected.set(Vec::new());
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

    let highlight: String = selected
        .iter()
        .map(|sid| {
            format!(
                ".diagram g.block[data-sid=\"{}\"] > :is(rect, polygon, ellipse) {{ stroke: #ff9e64; stroke-width: 3px; }}\n",
                css_string(sid)
            )
        })
        .collect();
    let wire_highlight = selected_wire
        .as_ref()
        .map(|dst| {
            format!(
                ".diagram polyline.wire-hit[data-dst-sid=\"{}\"][data-dst-kind=\"{}\"][data-dst-index=\"{}\"] {{ stroke: rgba(255, 158, 100, 0.55); }}",
                css_string(&dst.block.0),
                dst.port.kind.token(),
                dst.port.index
            )
        })
        .unwrap_or_default();
    let highlight = match &props.diff {
        Some(d) => diff_css(d, &path, system) + &highlight + &wire_highlight,
        None => highlight + &wire_highlight,
    };

    let canvas = match rendered.as_ref() {
        Ok(svg) => Html::from_html_unchecked(AttrValue::from(svg.clone())),
        Err(e) => html! { <div class="error">{ e }</div> },
    };
    // Drawn over the diagram in its units: the connection being dragged,
    // and the selected block's resize handles and new outline.
    let overlay = match (*rendered).as_ref().ok().and_then(|s| view_box(s)) {
        Some([vx, vy, vw, vh]) => {
            let wire = wire_preview.map(|(a, b)| html! {
                <line class="wire-preview" x1={a.x.to_string()} y1={a.y.to_string()} x2={b.x.to_string()} y2={b.y.to_string()} />
            });
            let handles = selected_block.filter(|_| props.on_edit.is_some()).map(|b| {
                let r = resize_preview.unwrap_or(b.position);
                // Constant on screen whatever the zoom.
                let size = 8.0 / view.scale;
                let corners = [
                    ("nw", r.left, r.top),
                    ("ne", r.right, r.top),
                    ("sw", r.left, r.bottom),
                    ("se", r.right, r.bottom),
                ];
                html! {
                    <>
                        if resize_preview.is_some() {
                            <rect class="resize-outline" x={r.left.to_string()} y={r.top.to_string()}
                                width={r.width().to_string()} height={r.height().to_string()} />
                        }
                        { for corners.iter().map(|(corner, x, y)| html! {
                            <rect class="resize-handle" data-sid={b.id.0.clone()} data-corner={*corner}
                                x={(x - size / 2.0).to_string()} y={(y - size / 2.0).to_string()}
                                width={size.to_string()} height={size.to_string()} />
                        }) }
                    </>
                }
            });
            let selecting = select_box.map(|(a, b)| html! {
                <rect class="select-box" x={a.x.min(b.x).to_string()} y={a.y.min(b.y).to_string()}
                    width={(a.x - b.x).abs().to_string()} height={(a.y - b.y).abs().to_string()} />
            });
            let rerouting = route_preview.as_ref().map(|pts| {
                let points: Vec<String> = pts.iter().map(|p| format!("{},{}", p.x, p.y)).collect();
                html! { <polyline class="route-preview" points={points.join(" ")} /> }
            });
            html! {
                <svg class="overlay" viewBox={format!("{vx} {vy} {vw} {vh}")}
                    width={vw.to_string()} height={vh.to_string()}>
                    { for wire }
                    { for handles }
                    { for selecting }
                    { for rerouting }
                </svg>
            }
        }
        None => html! {},
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
                        move |p: Vec<String>| { path.set(p); selected.set(Vec::new()); }
                    })} />
                    if props.on_edit.is_some() {
                        <BlockPalette />
                    }
                </aside>
                <div class={classes!("diagram", (*theme == Theme::Light).then_some("light"), props.on_edit.is_some().then_some("editing"))}
                    ref={container} tabindex="0"
                    {onmousedown} {onmousemove} onmouseup={end_drag.clone()} onmouseleave={end_drag}
                    {onclick} {ondblclick} {onkeydown} {ondragover} {ondrop}>
                    <style>{ highlight }</style>
                    <div class="canvas" style={transform}>{ canvas }{ overlay }</div>
                </div>
                if let Some(b) = selected_block {
                    <Inspector block={Rc::new(b.clone())} chart={selected_chart.map(|c| Rc::new(c.clone()))}
                        system={system_ref.clone()} lines={Rc::new(system.map(|s| s.lines.clone()).unwrap_or_default())}
                        on_edit={on_edit.clone()} on_open={Callback::from({
                        let path = path.clone();
                        let selected = selected.clone();
                        move |name: String| {
                            let mut p = (*path).clone();
                            p.push(name);
                            path.set(p);
                            selected.set(Vec::new());
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

/// Native blocks that can be dragged onto the diagram, by category.
#[function_component(BlockPalette)]
fn block_palette() -> Html {
    let groups = catalog::PALETTE_CATEGORIES.iter().map(|category| {
        let items = catalog::blocks_in_category(category).map(|b| {
            let type_key = b.type_key;
            let ondragstart = Callback::from(move |e: DragEvent| {
                if let Some(d) = e.data_transfer() {
                    let _ = d.set_data(PALETTE_DRAG, type_key);
                    d.set_effect_allowed("copy");
                }
            });
            html! {
                <li class="palette-block" draggable="true" {ondragstart} title={b.type_key}>
                    { b.label }
                </li>
            }
        });
        html! {
            <>
                <h4>{ *category }</h4>
                <ul>{ for items }</ul>
            </>
        }
    });
    html! {
        <div class="palette">
            <h3>{ "Blocks" }</h3>
            <p class="hint">{ "Drag onto the diagram. Drag from a port to another to connect; select a line and press Delete to remove it." }</p>
            { for groups }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct InspectorProps {
    block: Rc<Block>,
    /// Stateflow chart implementing the block.
    chart: Option<Rc<Chart>>,
    /// The system containing the block.
    system: SystemRef,
    /// Lines of that system, to tell whether deleting the block cuts any.
    lines: Rc<Vec<Line>>,
    on_open: Callback<String>,
    on_edit: Option<Callback<Edit>>,
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
    // A value cell: editable input when editing, code otherwise. Changes
    // are committed on blur or Enter.
    let value_cell = |name: &str, value: &str| -> Html {
        match &props.on_edit {
            Some(on_edit) => {
                let (on_edit, system, id, name, current) = (
                    on_edit.clone(),
                    props.system.clone(),
                    b.id.clone(),
                    name.to_string(),
                    value.to_string(),
                );
                let onchange = Callback::from(move |e: Event| {
                    let value = e.target_unchecked_into::<HtmlInputElement>().value();
                    if value != current {
                        on_edit.emit(Edit::SetParameter {
                            system: system.clone(),
                            id: id.clone(),
                            name: name.clone(),
                            value,
                        });
                    }
                });
                html! { <input class="param" value={value.to_string()} {onchange} /> }
            }
            None => html! { <code>{ value }</code> },
        }
    };
    let title = match &props.on_edit {
        Some(on_edit) => {
            let (on_edit, system, id, current) = (
                on_edit.clone(),
                props.system.clone(),
                b.id.clone(),
                b.name.clone(),
            );
            let onchange = Callback::from(move |e: Event| {
                let name = e.target_unchecked_into::<HtmlInputElement>().value();
                if name != current && !name.trim().is_empty() {
                    on_edit.emit(Edit::RenameBlock {
                        system: system.clone(),
                        id: id.clone(),
                        name,
                    });
                }
            });
            html! { <input class="block-name" value={b.name.clone()} {onchange} /> }
        }
        None => html! { <h3>{ b.name.replace('\n', " ") }</h3> },
    };
    let delete = props.on_edit.clone().map(|on_edit| {
        let (system, id, lines) = (props.system.clone(), b.id.clone(), props.lines.clone());
        let onclick = Callback::from(move |_: MouseEvent| {
            for edit in
                confirm_delete(&system, std::slice::from_ref(&id), &lines).unwrap_or_default()
            {
                on_edit.emit(edit);
            }
        });
        html! { <button class="danger" {onclick}>{ "Delete block" }</button> }
    });
    html! {
        <aside class="inspector">
            { title }
            <div class="muted">{ format!("{kind} · SID {}", b.id) }</div>
            { for open }
            { for delete }
            if let Some(script) = script {
                <h4>{ "MATLAB code" }</h4>
                <pre class="script"><code>{ script }</code></pre>
            }
            if let Some(src) = &b.library_source {
                <div class="muted">{ format!("Library: {}", src.replace('\n', " ")) }</div>
            }
            if let Some(i) = &b.interface {
                <h4>{ "Bus element port" }</h4>
                <table>
                    <tr><td>{ "Port" }</td><td>{ i.port_number.map_or("?".to_string(), |n| n.to_string()) }</td></tr>
                    if let Some(name) = &i.port_name {
                        <tr><td>{ "Port name" }</td><td>{ name }</td></tr>
                    }
                    if let Some(element) = &i.element {
                        <tr><td>{ "Element" }</td><td><code>{ element }</code></td></tr>
                    }
                </table>
            }
            if let Some(mask) = &b.mask {
                <h4>{ "Mask parameters" }</h4>
                <table>
                    { for mask.parameters.iter().map(|p| html! {
                        <tr><td title={p.prompt.clone().unwrap_or_default()}>{ &p.name }</td><td>{ value_cell(&p.name, &p.value) }</td></tr>
                    }) }
                </table>
            }
            { for ScopeConfig::from_block(b).map(scope_section) }
            <h4>{ "Parameters" }</h4>
            <table>
                { for b.parameters.iter().filter(|(k, _)| !HIDDEN_PARAMETERS.contains(&k.as_str())).map(|(k, v)| html! {
                    <tr><td>{ k }</td><td>{ value_cell(k, v) }</td></tr>
                }) }
            </table>
            if let Some(spec) = b.param("ScopeSpecificationString") {
                <details class="raw">
                    <summary>{ "Raw scope specification" }</summary>
                    <pre class="script"><code>{ spec }</code></pre>
                </details>
            }
        </aside>
    }
}

/// Parameters not listed in the inspector table: editor bookkeeping, and
/// the scope specification, which is shown structured instead.
const HIDDEN_PARAMETERS: &[&str] = &["ZOrder", "ScopeSpecificationString"];

fn fmt_num(v: f64) -> String {
    if v != 0.0 && (v.abs() < 1e-3 || v.abs() >= 1e6) {
        format!("{v:e}")
    } else {
        format!("{v}")
    }
}

fn on_off(v: Option<bool>) -> Option<&'static str> {
    v.map(|b| if b { "on" } else { "off" })
}

/// The Scope settings of a block in readable form.
fn scope_section(config: Result<ScopeConfig, String>) -> Html {
    let c = match config {
        Ok(c) => c,
        Err(e) => {
            return html! {
                <>
                    <h4>{ "Scope" }</h4>
                    <p class="error">{ format!("Unreadable scope specification: {e}") }</p>
                </>
            }
        }
    };
    let row = |k: &str, v: Option<String>| -> Html {
        match v {
            Some(v) => html! { <tr><td>{ k }</td><td>{ v }</td></tr> },
            None => html! {},
        }
    };
    let window = c
        .window
        .map(|[_, _, w, h]| format!("{}×{} px", fmt_num(w), fmt_num(h)));
    html! {
        <>
            <h4>{ "Scope" }</h4>
            <table class="scope">
                { row("Configured logging variable", c.logging_variable.clone()) }
                { row("Saved logging status", Some(on_off(c.logging_enabled).unwrap_or("not specified").to_string())) }
                { row("Time span", c.time_span.map(|t| format!("{} s", fmt_num(t)))) }
                { row("Window", window) }
                { row("Opens with model", on_off(c.open_at_start).map(str::to_string)) }
                { row("Saved by", c.version.clone().map(|v| format!("Simulink {v}"))) }
            </table>
            if c.displays.is_empty() {
                <p class="muted">{ "No saved display settings found." }</p>
            }
            { for c.displays.iter().enumerate().map(|(i, d)| {
                let limits = match (d.y_min, d.y_max) {
                    (None, None) => None,
                    (lo, hi) => Some(format!(
                        "{} … {}",
                        lo.map_or("auto".into(), fmt_num),
                        hi.map_or("auto".into(), fmt_num)
                    )),
                };
                let grid = match (d.x_grid, d.y_grid) {
                    (None, None) => None,
                    (x, y) => Some(format!("x {}, y {}", on_off(x).unwrap_or("?"), on_off(y).unwrap_or("?"))),
                };
                html! {
                    <table class="scope">
                        if c.displays.len() > 1 {
                            <tr><th colspan="2">{ format!("Display {}", i + 1) }</th></tr>
                        }
                        { row("Title", d.title.clone()) }
                        { row("Signals", (!d.line_names.is_empty()).then(|| d.line_names.join(", "))) }
                        { row("Y limits", limits) }
                        { row("Y label", d.y_label.clone()) }
                        { row("Legend", on_off(d.legend).map(str::to_string)) }
                        { row("Grid", grid) }
                    </table>
                }
            }) }
        </>
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
