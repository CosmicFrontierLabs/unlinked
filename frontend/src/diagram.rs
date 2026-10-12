//! Interactive diagram viewer: pan/zoom, subsystem drill-down, block
//! inspector. The diagram itself is SVG produced by `unlinked-render` in
//! wasm; clicks are resolved through the `data-*` attributes it emits.
//! Opening a Stateflow block shows its chart (or MATLAB Function code)
//! instead of the generated plumbing inside it; path entries past the chart
//! block name subcharted states.

use crate::dialog::ParameterDialog;
use crate::find::{FindBlocks, Found};
use crate::icons;
use crate::palette::{BlockPalette, Pick, QuickInsert, PALETTE_DRAG};
use crate::results::{ResultsContext, ShownRun};
use gloo_events::{EventListener, EventListenerOptions};
use std::rc::Rc;
use unlinked_model::diff::{BlockChange, ModelDiff};
use unlinked_model::edit::{
    system_ids, system_names, touches, AnnotationTarget, DisconnectPolicy, Edit, SystemRef,
};
use unlinked_model::scope::ScopeConfig;
use unlinked_model::validation::{Diagnostic, DiagnosticTarget, Severity};
use unlinked_model::{catalog, geometry};
use unlinked_model::{
    Annotation, Block, BlockId, Chart, Endpoint, Line, Model, Point, PortKind, PortRef, Rect,
    System,
};
use unlinked_render::{render_chart_view_svg, render_svg, RenderOptions, Theme};
use wasm_bindgen::JsCast;
use web_sys::{
    DragEvent, Element, HtmlElement, HtmlInputElement, HtmlTextAreaElement, KeyboardEvent,
    MouseEvent, TouchEvent, WheelEvent,
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
    /// Problems found in the model: outlined on the diagram, listed by the
    /// toolbar's problems button and shown in the block inspector.
    #[prop_or_default]
    pub problems: Rc<Vec<Diagnostic>>,
    /// The check stopped listing problems at its limits, or did not finish.
    #[prop_or_default]
    pub problems_truncated: bool,
    /// Starts a compile check of the shown model; unset while one runs.
    #[prop_or_default]
    pub on_compile: Option<Callback<()>>,
    /// The state or outcome of the last compile check, if any.
    #[prop_or_default]
    pub compile_status: AttrValue,
    /// Opens the model settings, for problems with them.
    #[prop_or_default]
    pub on_settings: Option<Callback<()>>,
    /// Whether the view is shown. A hidden view has no size, so fitting
    /// waits until it is shown again.
    #[prop_or(true)]
    pub active: bool,
    /// Tools shown first on the toolbar, such as undo and redo.
    #[prop_or_default]
    pub leading: Html,
    /// Tools shown last on the toolbar, such as saving.
    #[prop_or_default]
    pub trailing: Html,
}

/// Grid block positions snap to when dragged.
const SNAP: f64 = 5.0;

/// How near a port, in screen pixels, a dragged connection's end snaps to
/// it.
const SNAP_RADIUS: f64 = 24.0;

/// How near, in screen pixels, dragged blocks must come to lining up with
/// another for the smart guides to line them up.
const GUIDE_REACH: f64 = 6.0;

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
        target: RouteTarget,
        points: Vec<Point>,
        fixed: usize,
        segment: usize,
        start: Point,
        /// The new stored vertices once the drag has moved anything.
        route: Option<Vec<Point>>,
    },
    /// Dragging a new connection out of port `from`, toward `target` if it
    /// is near enough to one to connect.
    Wire {
        from: Endpoint,
        target: Option<Endpoint>,
    },
    /// Moving annotation `index` (its rendered elements) by `offset`.
    Annotation {
        sx: f64,
        sy: f64,
        index: usize,
        elements: Vec<Element>,
        offset: (f64, f64),
        moved: bool,
    },
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

/// An editing action, whether from the keyboard, the toolbar or the context
/// menu.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    Cut,
    Copy,
    Paste,
    /// Copy and paste the selection in one step.
    Duplicate,
    Rotate,
    Flip,
    Group,
    Expand,
    Delete,
    /// Edit the name of the lone selected block.
    Rename,
    SelectAll,
    /// Move the selected blocks by (dx, dy).
    Nudge(f64, f64),
    /// Line the selected blocks up, or space them evenly.
    Arrange(Arrange),
    /// Drop the selected line's stored route, so it is routed afresh.
    AutoRoute,
}

/// How selected blocks are lined up or spaced, as Simulink's Format menu
/// offers.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Arrange {
    Left,
    Center,
    Right,
    Top,
    Middle,
    Bottom,
    /// Equal gaps between neighbours, left to right.
    Across,
    /// Equal gaps between neighbours, top to bottom.
    Down,
}

impl Arrange {
    /// Blocks needed for it to do anything.
    fn needs(self) -> usize {
        match self {
            Arrange::Across | Arrange::Down => 3,
            _ => 2,
        }
    }
}

/// Where `blocks` go when arranged `how`: lined up on the first of them in
/// the relevant direction (the leftmost for Left, ...), or spread between
/// the outermost two keeping their sizes. Blocks that stay put are left out.
fn arranged(blocks: &[(BlockId, Rect)], how: Arrange) -> Vec<(BlockId, Rect)> {
    let shift =
        |r: Rect, dx: f64, dy: f64| Rect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy);
    let cx = |r: &Rect| (r.left + r.right) / 2.0;
    let cy = |r: &Rect| (r.top + r.bottom) / 2.0;
    let fold = |f: &dyn Fn(&Rect) -> f64, min: bool| {
        blocks.iter().map(|(_, r)| f(r)).fold(
            if min {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            },
            |a, b| if min { a.min(b) } else { a.max(b) },
        )
    };
    let moved: Vec<(BlockId, Rect)> = match how {
        Arrange::Left | Arrange::Right | Arrange::Center => {
            let at = match how {
                Arrange::Left => fold(&|r| r.left, true),
                Arrange::Right => fold(&|r| r.right, false),
                _ => blocks.first().map_or(0.0, |(_, r)| cx(r)),
            };
            blocks
                .iter()
                .map(|(id, r)| {
                    let dx = match how {
                        Arrange::Left => at - r.left,
                        Arrange::Right => at - r.right,
                        _ => at - cx(r),
                    };
                    (id.clone(), shift(*r, dx, 0.0))
                })
                .collect()
        }
        Arrange::Top | Arrange::Bottom | Arrange::Middle => {
            let at = match how {
                Arrange::Top => fold(&|r| r.top, true),
                Arrange::Bottom => fold(&|r| r.bottom, false),
                _ => blocks.first().map_or(0.0, |(_, r)| cy(r)),
            };
            blocks
                .iter()
                .map(|(id, r)| {
                    let dy = match how {
                        Arrange::Top => at - r.top,
                        Arrange::Bottom => at - r.bottom,
                        _ => at - cy(r),
                    };
                    (id.clone(), shift(*r, 0.0, dy))
                })
                .collect()
        }
        Arrange::Across | Arrange::Down => {
            let across = how == Arrange::Across;
            let mut sorted: Vec<&(BlockId, Rect)> = blocks.iter().collect();
            sorted.sort_by(|(_, a), (_, b)| {
                let (a, b) = if across {
                    (a.left, b.left)
                } else {
                    (a.top, b.top)
                };
                a.total_cmp(&b)
            });
            let (Some((_, first)), Some((_, last))) = (sorted.first(), sorted.last()) else {
                return Vec::new();
            };
            let (start, end) = if across {
                (first.left, last.right)
            } else {
                (first.top, last.bottom)
            };
            let sizes: f64 = sorted
                .iter()
                .map(|(_, r)| if across { r.width() } else { r.height() })
                .sum();
            let gap = (end - start - sizes) / (sorted.len() - 1) as f64;
            let mut at = start;
            sorted
                .into_iter()
                .map(|(id, r)| {
                    let moved = if across {
                        shift(*r, at - r.left, 0.0)
                    } else {
                        shift(*r, 0.0, at - r.top)
                    };
                    at += if across { r.width() } else { r.height() } + gap;
                    (id.clone(), moved)
                })
                .collect()
        }
    };
    // On whole diagram units, as everything else places blocks.
    moved
        .into_iter()
        .map(|(id, r)| {
            let (dx, dy) = (r.left.round() - r.left, r.top.round() - r.top);
            (id, shift(r, dx, dy))
        })
        .filter(|(id, r)| blocks.iter().any(|(b, was)| b == id && was != r))
        .collect()
}

/// Actions on the editing toolbar, in groups, and in the context menu, in
/// order.
const TOOLBAR: [&[Action]; 3] = [
    &[Action::Cut, Action::Copy, Action::Paste, Action::Delete],
    &[Action::Rotate, Action::Flip],
    &[Action::Group, Action::Expand],
];
const MENU: [Action; 20] = [
    Action::Cut,
    Action::Copy,
    Action::Paste,
    Action::Duplicate,
    Action::Delete,
    Action::Rename,
    Action::AutoRoute,
    Action::Rotate,
    Action::Flip,
    Action::Group,
    Action::Expand,
    Action::Arrange(Arrange::Left),
    Action::Arrange(Arrange::Center),
    Action::Arrange(Arrange::Right),
    Action::Arrange(Arrange::Top),
    Action::Arrange(Arrange::Middle),
    Action::Arrange(Arrange::Bottom),
    Action::Arrange(Arrange::Across),
    Action::Arrange(Arrange::Down),
    Action::SelectAll,
];

/// What a click on `target` selects: a wire, a trunk, a block, an
/// annotation of `annotations`, or nothing.
fn hit(target: Option<web_sys::EventTarget>, annotations: &[Annotation]) -> Selection {
    if let Some(dst) =
        closest(target.clone(), "polyline.wire-hit").and_then(|w| endpoint_of(&w, "dst-"))
    {
        return Selection::Wire(dst);
    }
    if let Some(src) =
        closest(target.clone(), "polyline.trunk-hit").and_then(|w| endpoint_of(&w, "src-"))
    {
        return Selection::Trunk(src);
    }
    if let Some(id) = block_group(target.clone()).and_then(|g| g.get_attribute("data-sid")) {
        return Selection::Blocks(vec![id]);
    }
    closest(target, "[data-annotation]")
        .and_then(|a| a.get_attribute("data-annotation")?.parse::<usize>().ok())
        .and_then(|i| Some(Selection::Annotation(i, annotations.get(i)?.clone())))
        .unwrap_or_default()
}

/// Whether `action` applies to `selection`; the keyboard, the toolbar and
/// the context menu all offer exactly these. `subsystem` tells whether the
/// selection is one subsystem block.
fn available(action: Action, selection: &Selection, clipboard: bool, subsystem: bool) -> bool {
    let blocks = !selection.blocks().is_empty();
    match action {
        Action::Cut
        | Action::Copy
        | Action::Duplicate
        | Action::Rotate
        | Action::Flip
        | Action::Group
        | Action::Nudge(..) => blocks,
        Action::Rename => selection.blocks().len() == 1,
        Action::Arrange(how) => selection.blocks().len() >= how.needs(),
        Action::AutoRoute => matches!(selection, Selection::Wire(_) | Selection::Trunk(_)),
        Action::Expand => subsystem,
        Action::Paste => clipboard,
        Action::SelectAll => true,
        Action::Delete => {
            blocks || matches!(selection, Selection::Wire(_) | Selection::Annotation(..))
        }
    }
}

/// A button performing `action`: a toolbar button, or a context menu item
/// showing its shortcut.
fn action_button(action: Action, enabled: bool, act: &Callback<Action>, menu: bool) -> Html {
    let (label, shortcut) = action.label();
    let act = act.clone();
    let onclick = Callback::from(move |_: MouseEvent| act.emit(action));
    if menu {
        html! {
            <button class="menu-item" {onclick}>
                <span>{ label }</span><span class="shortcut">{ shortcut }</span>
            </button>
        }
    } else {
        html! {
            <button class="tool icon" {onclick} disabled={!enabled}
                title={format!("{label} ({shortcut})")} aria-label={label}>
                { action.icon() }
            </button>
        }
    }
}

impl Action {
    fn icon(self) -> Html {
        match self {
            Action::Cut => icons::cut(),
            Action::Copy => icons::copy(),
            Action::Paste => icons::paste(),
            Action::Rotate => icons::rotate(),
            Action::Flip => icons::flip(),
            Action::Group => icons::group(),
            Action::Expand => icons::expand(),
            Action::Delete => icons::delete(),
            Action::Duplicate
            | Action::Rename
            | Action::SelectAll
            | Action::Nudge(..)
            | Action::Arrange(_)
            | Action::AutoRoute => html! {},
        }
    }

    /// Label and shortcut, as shown on buttons and in the context menu.
    fn label(self) -> (&'static str, &'static str) {
        match self {
            Action::Cut => ("Cut", "Ctrl+X"),
            Action::Copy => ("Copy", "Ctrl+C"),
            Action::Paste => ("Paste", "Ctrl+V"),
            Action::Duplicate => ("Duplicate", "Ctrl+D"),
            Action::Rename => ("Rename", "F2"),
            Action::Rotate => ("Rotate", "Ctrl+R"),
            Action::Flip => ("Flip", "Ctrl+I"),
            Action::Group => ("Group into subsystem", "Ctrl+G"),
            Action::Expand => ("Expand subsystem", "Ctrl+Shift+G"),
            Action::Delete => ("Delete", "Del"),
            Action::SelectAll => ("Select all", "Ctrl+A"),
            Action::Nudge(..) => ("Nudge", "Arrows"),
            Action::AutoRoute => ("Auto-route line", ""),
            Action::Arrange(how) => (
                match how {
                    Arrange::Left => "Align left edges",
                    Arrange::Center => "Align centres",
                    Arrange::Right => "Align right edges",
                    Arrange::Top => "Align top edges",
                    Arrange::Middle => "Align middles",
                    Arrange::Bottom => "Align bottom edges",
                    Arrange::Across => "Space evenly across",
                    Arrange::Down => "Space evenly down",
                },
                "",
            ),
        }
    }
}

/// What is selected on the shown diagram level: one kind of thing at a time,
/// so actions such as Delete never act on a stale selection of another kind.
#[derive(Clone, Default, PartialEq)]
enum Selection {
    #[default]
    Nothing,
    /// Blocks, by SID; the inspector shows a lone block.
    Blocks(Vec<String>),
    /// A connection, by the input it drives.
    Wire(Endpoint),
    /// A branched line, by the source of its trunk.
    Trunk(Endpoint),
    /// An annotation, by its index in the system and its content when
    /// selected: the index alone could later name another annotation (after
    /// a delete and an undo, say), and then the selection lapses.
    Annotation(usize, Annotation),
}

impl Selection {
    /// The selected blocks, if blocks are what is selected.
    fn blocks(&self) -> &[String] {
        match self {
            Selection::Blocks(ids) => ids,
            _ => &[],
        }
    }

    /// The selected annotation, if `sys` still holds it at its index.
    fn annotation<'a>(&self, sys: &'a System) -> Option<(usize, &'a Annotation)> {
        match self {
            Selection::Annotation(i, expected) => sys
                .annotations
                .get(*i)
                .filter(|a| *a == expected)
                .map(|a| (*i, a)),
            _ => None,
        }
    }
}

/// A wire whose route is being dragged.
enum RouteTarget {
    /// The final leg into input `0`, ending at the port's anchor and outline.
    Leaf(Endpoint),
    /// The trunk of the branched line from output `0`, ending at the
    /// junction, which stays where it is.
    Trunk(Endpoint),
}

impl RouteTarget {
    /// Trailing drawn points that are not free to move.
    fn fixed_end(&self) -> usize {
        match self {
            RouteTarget::Leaf(_) => 2,
            RouteTarget::Trunk(_) => 1,
        }
    }
}

/// Whether a line may start at a port of this kind.
fn is_source(kind: PortKind) -> bool {
    matches!(kind, PortKind::Out | PortKind::State)
}

/// Port kinds a connection can be dragged to, as the renderer marks them.
const WIRABLE: [PortKind; 7] = [
    PortKind::In,
    PortKind::Out,
    PortKind::Enable,
    PortKind::Trigger,
    PortKind::State,
    PortKind::IfAction,
    PortKind::Reset,
];

/// The port a connection dragged from `from` would end at, and its anchor:
/// the nearest port of the other direction within `radius` of `at`, or, over
/// block `over`, that block's nearest such port. Inputs already driven and
/// `from` itself are skipped.
fn snap_port(
    sys: &System,
    from: &Endpoint,
    at: Point,
    radius: f64,
    over: Option<&BlockId>,
) -> Option<(Endpoint, Point)> {
    let want_source = !is_source(from.port.kind);
    let mut best: Option<(f64, Endpoint, Point)> = None;
    for b in &sys.blocks {
        if over.is_some_and(|id| *id != b.id) {
            continue;
        }
        for kind in WIRABLE.into_iter().filter(|k| is_source(*k) == want_source) {
            for index in 1..=b.ports.count(kind).min(64) {
                let ep = Endpoint {
                    block: b.id.clone(),
                    port: PortRef { kind, index },
                };
                if ep == *from
                    || (!want_source
                        && sys
                            .lines
                            .iter()
                            .any(|l| unlinked_model::edit::drives(l, &ep)))
                {
                    continue;
                }
                let p = geometry::port_anchor(b, ep.port);
                let d = ((p.x - at.x).powi(2) + (p.y - at.y).powi(2)).sqrt();
                if (over.is_some() || d <= radius) && best.as_ref().is_none_or(|(bd, ..)| d < *bd) {
                    best = Some((d, ep, p));
                }
            }
        }
    }
    best.map(|(_, ep, p)| (ep, p))
}

/// The connection Ctrl-clicking block `to` with block `from` selected makes:
/// from `from`'s first output not yet connected (else its first, to branch
/// it) to `to`'s first undriven input.
fn auto_connect(sys: &System, from: &BlockId, to: &BlockId) -> Option<(Endpoint, Endpoint)> {
    let (a, b) = (sys.block(from)?, sys.block(to)?);
    let port = |block: &BlockId, kind, index| Endpoint {
        block: block.clone(),
        port: PortRef { kind, index },
    };
    let outputs: Vec<Endpoint> = (1..=a.ports.outputs.min(64))
        .map(|i| port(from, PortKind::Out, i))
        .collect();
    let src = outputs
        .iter()
        .find(|ep| !sys.lines.iter().any(|l| l.src.as_ref() == Some(*ep)))
        .or(outputs.first())?
        .clone();
    let dst = (1..=b.ports.inputs.min(64))
        .map(|i| port(to, PortKind::In, i))
        .find(|ep| {
            !sys.lines
                .iter()
                .any(|l| unlinked_model::edit::drives(l, ep))
        })?;
    Some((src, dst))
}

/// Where a context menu of `items` rows opened at client point (x, y) is
/// drawn: below and right of the point, moved up and left to stay in the
/// window, and scrolling only if the window is shorter than the menu.
fn menu_place(x: i32, y: i32, items: usize) -> String {
    const ROW: f64 = 29.0;
    const WIDTH: f64 = 240.0;
    const MARGIN: f64 = 8.0;
    let window = web_sys::window();
    let size = |v: Option<wasm_bindgen::JsValue>| v.and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
    let width = size(window.as_ref().and_then(|w| w.inner_width().ok()));
    let height = size(window.as_ref().and_then(|w| w.inner_height().ok()));
    let tall = items as f64 * ROW + 2.0 * MARGIN;
    let left = (x as f64).min(width - WIDTH - MARGIN).max(0.0);
    let top = (y as f64).min(height - tall - MARGIN).max(MARGIN);
    format!(
        "left: {left}px; top: {top}px; max-height: {}px;",
        (height - top - MARGIN).max(ROW)
    )
}

/// Width below which panels lie over the canvas instead of beside it; the
/// stylesheet's narrow layout uses the same.
const NARROW: f64 = 760.0;

/// Whether the window is narrow enough for panels to cover the diagram.
fn narrow() -> bool {
    gloo_utils::window()
        .inner_width()
        .ok()
        .and_then(|w| w.as_f64())
        .is_some_and(|w| w < NARROW)
}

/// A canvas preference kept in the browser between visits.
fn pref(key: &str) -> Option<String> {
    gloo_utils::window()
        .local_storage()
        .ok()??
        .get_item(&format!("unlinked.canvas.{key}"))
        .ok()?
}

fn set_pref(key: &str, value: &str) {
    if let Ok(Some(storage)) = gloo_utils::window().local_storage() {
        let _ = storage.set_item(&format!("unlinked.canvas.{key}"), value);
    }
}

/// A smart guide shown while dragging: blocks lined up along a horizontal
/// (`y`) or vertical (`x`) line, from `from` to `to` along it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Guide {
    Horizontal { y: f64, from: f64, to: f64 },
    Vertical { x: f64, from: f64, to: f64 },
}

/// Smart guides for blocks `moving` of `sys` dragged by `offset`: the
/// offset adjusted, within `reach` on each axis, to line a moving block up
/// with a block staying put, and the guides showing how. Ports facing each
/// other are lined up so the line between them runs straight, as are
/// centres and edges.
fn align(
    sys: &System,
    moving: &[BlockId],
    offset: (f64, f64),
    reach: f64,
) -> ((f64, f64), Vec<Guide>) {
    let shifted = |r: Rect, (dx, dy): (f64, f64)| {
        Rect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy)
    };
    // Points worth lining up on a block moved by `by`: its ports, centre and
    // corners. A move keeps a block's size, so its ports move with it.
    let marks = |b: &Block, by: (f64, f64)| -> Vec<Point> {
        let at = shifted(b.position, by);
        let mut out: Vec<Point> = [PortKind::In, PortKind::Out]
            .into_iter()
            .flat_map(|kind| {
                (1..=b.ports.count(kind).min(64)).map(move |index| PortRef { kind, index })
            })
            .map(|port| {
                let p = geometry::port_anchor(b, port);
                Point::new(p.x + by.0, p.y + by.1)
            })
            .collect();
        let c = Point::new((at.left + at.right) / 2.0, (at.top + at.bottom) / 2.0);
        out.extend([
            c,
            Point::new(at.left, at.top),
            Point::new(at.right, at.bottom),
        ]);
        out
    };
    let (moved, fixed): (Vec<&Block>, Vec<&Block>) =
        sys.blocks.iter().partition(|b| moving.contains(&b.id));
    let fixed: Vec<(Rect, Vec<Point>)> = fixed
        .iter()
        .map(|f| (f.position, marks(f, (0.0, 0.0))))
        .collect();
    // The smallest change on each axis that lines a mark up, with what it
    // lines up.
    let mut best_x: Option<(f64, f64, Rect, Rect)> = None;
    let mut best_y: Option<(f64, f64, Rect, Rect)> = None;
    for m in &moved {
        let at = shifted(m.position, offset);
        let ours = marks(m, offset);
        for (position, theirs) in &fixed {
            for p in &ours {
                for q in theirs {
                    let (dx, dy) = (q.x - p.x, q.y - p.y);
                    if dx.abs() <= reach && best_x.is_none_or(|(d, ..)| dx.abs() < d.abs()) {
                        best_x = Some((dx, q.x, at, *position));
                    }
                    if dy.abs() <= reach && best_y.is_none_or(|(d, ..)| dy.abs() < d.abs()) {
                        best_y = Some((dy, q.y, at, *position));
                    }
                }
            }
        }
    }
    let mut guides = Vec::new();
    let mut adjusted = offset;
    if let Some((dx, x, a, b)) = best_x {
        adjusted.0 += dx;
        let (a, b) = (shifted(a, (dx, 0.0)), b);
        guides.push(Guide::Vertical {
            x,
            from: a.top.min(b.top),
            to: a.bottom.max(b.bottom),
        });
    }
    if let Some((dy, y, a, b)) = best_y {
        adjusted.1 += dy;
        let (a, b) = (shifted(a, (0.0, dy)), b);
        guides.push(Guide::Horizontal {
            y,
            from: a.left.min(b.left),
            to: a.right.max(b.right),
        });
    }
    (adjusted, guides)
}

/// An orthogonal path from `a` to `b` turning halfway across, as a new
/// connection is previewed.
fn elbow(a: Point, b: Point) -> Vec<Point> {
    let mid = (a.x + b.x) / 2.0;
    vec![a, Point::new(mid, a.y), Point::new(mid, b.y), b]
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
    /// Pastes so far into each system; each into the same one lands
    /// further down and right.
    pastes: std::collections::HashMap<SystemRef, u32>,
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

/// Outline the shown system's blocks that have problems, and the subsystems
/// holding blocks that do; errors win over warnings.
fn problems_css(problems: &[Diagnostic], system: &[BlockId]) -> String {
    let mut worst: std::collections::BTreeMap<&str, (Severity, bool)> = Default::default();
    for d in problems {
        let DiagnosticTarget::Block { system: at, id, .. } = &d.target else {
            continue;
        };
        let (sid, inside) = match at.strip_prefix(system) {
            Some([]) => (id.0.as_str(), false),
            Some([sub, ..]) => (sub.0.as_str(), true),
            None => continue,
        };
        let entry = worst.entry(sid).or_insert((d.severity, inside));
        if d.severity == Severity::Error {
            entry.0 = Severity::Error;
        }
    }
    worst
        .into_iter()
        .map(|(sid, (severity, inside))| {
            format!(
                ".diagram g.block[data-sid=\"{}\"] > :is(rect, polygon, ellipse) {{ stroke: {}; stroke-width: 2.5px;{} }}\n",
                css_string(sid),
                match severity {
                    Severity::Error => "#f7768e",
                    Severity::Warning => "#e0af68",
                },
                if inside { " stroke-dasharray: 6 3;" } else { "" }
            )
        })
        .collect()
}

/// Where a problem is, in words.
fn problem_place(model: &Model, target: &DiagnosticTarget) -> String {
    match target {
        DiagnosticTarget::Model => "Model".into(),
        DiagnosticTarget::Config { parameter } => format!("Model settings · {parameter}"),
        DiagnosticTarget::Block {
            system,
            id,
            parameter,
        } => {
            let mut place = system_names(model, system).unwrap_or_default();
            let path: Vec<&str> = place.iter().map(String::as_str).collect();
            let name = model
                .system_at(&path)
                .and_then(|s| s.block(id))
                .map_or_else(|| format!("SID {id}"), |b| b.name.clone());
            place.push(name);
            let mut text = place.join(" / ").replace('\n', " ");
            if let Some(p) = parameter {
                text.push_str(&format!(" · {p}"));
            }
            text
        }
        DiagnosticTarget::Line { system, .. } => {
            let mut place = system_names(model, system).unwrap_or_default();
            place.push("line".into());
            place.join(" / ").replace('\n', " ")
        }
    }
}

fn problem_class(d: &Diagnostic) -> &'static str {
    match d.severity {
        Severity::Error => "problem error",
        Severity::Warning => "problem warning",
    }
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
fn confirm_delete(
    model: &Model,
    system: &SystemRef,
    ids: &[BlockId],
    lines: &[Line],
) -> Option<Vec<Edit>> {
    let attached = lines
        .iter()
        .filter(|l| ids.iter().any(|id| touches(l, id)))
        .count();
    // Port blocks of a subsystem whose port is wired outside it: deleting
    // them removes that outer connection too.
    let outside = ids
        .iter()
        .filter(|id| {
            let edit = Edit::DeleteBlock {
                system: system.clone(),
                id: (*id).clone(),
                disconnect: DisconnectPolicy::Disconnect,
            };
            matches!(
                unlinked_model::boundary::boundary_remap(model, &edit),
                Ok(Some(remap)) if unlinked_model::boundary::cuts_outside(model, &remap)
            )
        })
        .count();
    let disconnect = if attached == 0 && outside == 0 {
        DisconnectPolicy::Reject
    } else {
        let what = if ids.len() == 1 {
            "this block".to_string()
        } else {
            format!("these {} blocks", ids.len())
        };
        let mut cut = Vec::new();
        if attached > 0 {
            cut.push(format!(
                "the {attached} line{} connected to {}",
                if attached == 1 { "" } else { "s" },
                if ids.len() == 1 { "it" } else { "them" }
            ));
        }
        if outside > 0 {
            cut.push(format!(
                "the connection{} to {} outside the subsystem",
                if outside == 1 { "" } else { "s" },
                if outside == 1 {
                    "its port"
                } else {
                    "their ports"
                }
            ));
        }
        let message = format!("Delete {what} and {}?", cut.join(" and "));
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
    let selection = use_state(Selection::default);
    // The selection box being drawn, corner to corner.
    let select_box = use_state(|| None::<(Point, Point)>);
    // A connection being dragged: from where it starts to the pointer, or
    // to the port it would connect to (then the flag is set).
    let wire_preview = use_state(|| None::<(Point, Point, bool)>);
    // The lines of blocks being dragged, by index, drawn where the drop
    // would put them.
    let drag_lines = use_state(|| None::<(Vec<usize>, String)>);
    // Smart guides for the blocks being dragged.
    let guides = use_state(Vec::<Guide>::new);
    // The block under a resting pointer and where its card shows, in
    // container pixels.
    let hover = use_state(|| None::<(String, f64, f64)>);
    // The line under a resting pointer, by index, highlighted as a whole.
    let hover_line = use_state(|| None::<usize>);
    // Whether the model browser and library are shown beside the canvas.
    // Shown unless hidden before, or, never chosen, on a narrow screen where
    // it would cover the diagram.
    let side = use_state(|| match pref("side").as_deref() {
        Some(chosen) => chosen != "hidden",
        None => !narrow(),
    });
    // The run shown in the simulation pane, if any, and whether its values
    // label the diagram's lines.
    let results = use_context::<ResultsContext>();
    let show_values = use_state(|| pref("values").as_deref() != Some("hidden"));
    use_effect_with(*show_values, |shown| {
        set_pref("values", if *shown { "shown" } else { "hidden" })
    });
    // The outline of a block being resized.
    let resize_preview = use_state(|| None::<Rect>);
    // A wire being rerouted, as it would be drawn.
    let route_preview = use_state(|| None::<Vec<Point>>);
    let theme = use_state(|| match pref("theme").as_deref() {
        Some("light") => Theme::Light,
        _ => Theme::Dark,
    });
    use_effect_with(*theme, |theme| {
        set_pref(
            "theme",
            if *theme == Theme::Light {
                "light"
            } else {
                "dark"
            },
        )
    });
    let view = use_reducer(|| View {
        scale: 1.0,
        x: 0.0,
        y: 0.0,
    });
    // The view as last rendered, for long-lived listeners that start from
    // it rather than change it (a handle they capture keeps its old value).
    let view_now = use_mut_ref(|| *view);
    *view_now.borrow_mut() = *view;
    let drag = use_mut_ref(|| None::<Drag>);
    // Blocks copied with Ctrl+C.
    let clipboard = use_mut_ref(|| None::<Clipboard>);
    let refresh = use_force_update();
    // A block an action asked to create, selected once the model has it; a
    // refused action leaves the selection as it was.
    let select_created = use_mut_ref(|| None::<BlockId>);
    // The open context menu's client position.
    let menu = use_state(|| None::<(i32, i32)>);
    let show_problems = use_state(|| false);
    let help = use_state(|| false);
    // Whether the find box is open.
    let finding = use_state(|| false);
    // The open quick insert: where it shows in the container, in pixels,
    // and the diagram point it adds at.
    let quick = use_state(|| None::<(f64, f64, Point)>);
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

    // Fit whenever a new diagram level is shown, or once a hidden view that
    // needed fitting is shown.
    let needs_fit = use_mut_ref(|| false);
    {
        let view = view.clone();
        let container = container.clone();
        let rendered = rendered.clone();
        let needs_fit = needs_fit.clone();
        {
            let needs_fit = needs_fit.clone();
            use_effect_with((fit_key.clone(), (*path).clone()), move |_| {
                *needs_fit.borrow_mut() = true;
            });
        }
        use_effect_with(
            (fit_key.clone(), (*path).clone(), props.active),
            move |(_, _, active)| {
                if !*active || !*needs_fit.borrow() {
                    return;
                }
                // Laid out with a size, so the fit is not to a hidden box.
                let shown = container
                    .cast::<HtmlElement>()
                    .filter(|el| el.client_width() > 0 && el.client_height() > 0);
                if let (Some(el), Ok(svg)) = (shown, rendered.as_ref()) {
                    if let Some(size) = svg_size(svg) {
                        view.dispatch(ViewAction::Set(fit(&el, size)));
                        *needs_fit.borrow_mut() = false;
                    }
                }
            },
        );
    }

    // A selection belongs to the level it was made on, except a block that
    // was opened to be shown, such as a problem's.
    let select_on_open = use_mut_ref(|| None::<String>);
    {
        let (selection, select_on_open) = (selection.clone(), select_on_open.clone());
        use_effect_with((*path).clone(), move |_| {
            selection.set(match select_on_open.borrow_mut().take() {
                Some(id) => Selection::Blocks(vec![id]),
                None => Selection::Nothing,
            })
        });
    }
    // Select a block an action created, once it is in the shown system.
    {
        let (selection, select_created, model, path) = (
            selection.clone(),
            select_created.clone(),
            props.model.clone(),
            (*path).clone(),
        );
        // Every render: success and refusal both re-render, so the request
        // never outlives its action (and a later block reusing its SID is
        // not selected by mistake).
        use_effect(move || {
            let created = select_created.borrow_mut().take();
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            if let Some(id) = created.filter(|id| {
                model
                    .system_at(&refs)
                    .is_some_and(|s| s.block(id).is_some())
            }) {
                selection.set(Selection::Blocks(vec![id.0]));
            }
        });
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
    // Touch: one finger pans, two pinch to zoom about their midpoint; a tap
    // still clicks. Registered by hand to keep the page from scrolling.
    {
        let view = view.clone();
        let container = container.clone();
        use_effect_with((), move |_| {
            // The view and the touches (container pixels) a gesture began
            // with: one point to pan from, or two to pinch.
            let gesture = Rc::new(std::cell::RefCell::new(None::<(View, Vec<(f64, f64)>)>));
            let listeners = container.cast::<HtmlElement>().map(|el| {
                let points = {
                    let el = el.clone();
                    move |e: &TouchEvent| -> Vec<(f64, f64)> {
                        let rect = el.get_bounding_client_rect();
                        let list = e.touches();
                        (0..list.length().min(2))
                            .filter_map(|i| list.get(i))
                            .map(|t| {
                                (
                                    t.client_x() as f64 - rect.left(),
                                    t.client_y() as f64 - rect.top(),
                                )
                            })
                            .collect()
                    }
                };
                let restart = {
                    let (gesture, view, points) =
                        (gesture.clone(), view_now.clone(), points.clone());
                    move |e: &web_sys::Event| {
                        let Some(e) = e.dyn_ref::<TouchEvent>() else {
                            return;
                        };
                        // Touches on controls and panels over the canvas
                        // (a result list being scrolled, say) are theirs.
                        let over_control = closest(
                            e.target(),
                            "button, input, select, textarea, a, .quick-insert, .shortcut-help, nav.crumbs",
                        )
                        .is_some();
                        let now = points(e);
                        *gesture.borrow_mut() = (!now.is_empty() && !over_control)
                            .then(|| (*view.borrow(), now));
                    }
                };
                let moved = {
                    let (gesture, view) = (gesture.clone(), view.clone());
                    move |e: &web_sys::Event| {
                        let Some(e) = e.dyn_ref::<TouchEvent>() else {
                            return;
                        };
                        let now = points(e);
                        let Some((start, from)) = gesture.borrow().clone() else {
                            return;
                        };
                        e.prevent_default();
                        let mid = |p: &[(f64, f64)]| {
                            let n = p.len() as f64;
                            (
                                p.iter().map(|q| q.0).sum::<f64>() / n,
                                p.iter().map(|q| q.1).sum::<f64>() / n,
                            )
                        };
                        let spread = |p: &[(f64, f64)]| match p {
                            [a, b] => ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt(),
                            _ => 0.0,
                        };
                        if now.len() != from.len() {
                            return;
                        }
                        let k = if from.len() == 2 && spread(&from) > 0.0 {
                            ((start.scale * spread(&now) / spread(&from)).clamp(0.02, 20.0))
                                / start.scale
                        } else {
                            1.0
                        };
                        let (m0, m1) = (mid(&from), mid(&now));
                        view.dispatch(ViewAction::Set(View {
                            scale: start.scale * k,
                            x: m1.0 - (m0.0 - start.x) * k,
                            y: m1.1 - (m0.1 - start.y) * k,
                        }));
                    }
                };
                let active = EventListenerOptions::enable_prevent_default();
                [
                    EventListener::new(&el, "touchstart", restart.clone()),
                    EventListener::new(&el, "touchend", restart.clone()),
                    EventListener::new(&el, "touchcancel", restart),
                    EventListener::new_with_options(&el, "touchmove", active, moved),
                ]
            });
            move || drop(listeners)
        });
    }

    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    let system = props.model.system_at(&refs);
    // Edits address the shown system by block IDs, which survive renames.
    let system_ref: SystemRef = system_ids(&props.model, &path).unwrap_or_default();
    // Recorded values at the shown run's chosen sample, by source port, and
    // whether the diagram changed since that run.
    let run = results.as_ref().and_then(|r| r.run.clone());
    let stale = results
        .as_ref()
        .zip(run.as_ref())
        .is_some_and(|(r, run)| r.dirty || run.version != r.shown_version);
    // Which signals label which lines, found again only for another model,
    // level or set of recorded signals: moving the shown sample keeps them.
    let sources = {
        let (model, run) = (props.model.clone(), run.clone());
        use_memo(
            (
                Rc::as_ptr(&props.model) as usize,
                (*path).clone(),
                run.as_ref().map(|r| Rc::as_ptr(&r.signals) as usize),
            ),
            move |(_, path, _)| {
                let refs: Vec<&str> = path.iter().map(String::as_str).collect();
                match (model.system_at(&refs), run) {
                    (Some(sys), Some(run)) => port_sources(&model, path, sys, &run),
                    _ => Vec::new(),
                }
            },
        )
    };
    let values: Vec<(Point, String)> = match &run {
        Some(run) => sources
            .iter()
            .filter_map(|(p, indices)| Some((*p, value_label(run, indices)?)))
            .collect(),
        None => Vec::new(),
    };

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
        let (container, rendered, wire_preview, selection, select_box) = (
            container.clone(),
            rendered.clone(),
            wire_preview.clone(),
            selection.clone(),
            select_box.clone(),
        );
        let quick = quick.clone();
        let (model, shown) = (props.model.clone(), (*path).clone());
        Callback::from(move |e: MouseEvent| {
            // Pressing elsewhere closes the quick insert.
            if quick.is_some() {
                quick.set(None);
            }
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
            // Ctrl/Cmd-dragging a line branches it, as in Simulink: the new
            // connection comes from the line's source.
            let branch = on_edit
                .as_ref()
                .filter(|_| e.ctrl_key() || e.meta_key())
                .and_then(|_| {
                    let hit = closest(e.target(), "polyline.wire-hit, polyline.trunk-hit")?;
                    match endpoint_of(&hit, "src-") {
                        Some(src) => Some(src),
                        None => {
                            let dst = endpoint_of(&hit, "dst-")?;
                            let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
                            let sys = model.system_at(&refs)?;
                            unlinked_model::edit::line_into(sys, &dst)?.src.clone()
                        }
                    }
                });
            if let Some(from) = port.or(branch) {
                let start = (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg));
                wire_preview.set(start.map(|p| (p, p, false)));
                *drag.borrow_mut() = Some(Drag::Wire { from, target: None });
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
                let hit = closest(e.target(), "polyline.wire-hit, polyline.trunk-hit")?;
                let target = match endpoint_of(&hit, "dst-") {
                    Some(dst) => RouteTarget::Leaf(dst),
                    None => RouteTarget::Trunk(endpoint_of(&hit, "src-")?),
                };
                let fixed: usize = hit.get_attribute("data-fixed")?.parse().ok()?;
                let points = polyline_points(&hit);
                let start = (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg))?;
                let segment = nearest_segment(&points, fixed.saturating_sub(1), start)?;
                Some(Drag::Segment {
                    target,
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
            // Pressing on an annotation may start moving it.
            let annotation = on_edit.as_ref().and_then(|_| {
                closest(e.target(), "[data-annotation]")?
                    .get_attribute("data-annotation")?
                    .parse::<usize>()
                    .ok()
            });
            if let Some(index) = annotation {
                let selector = format!("[data-annotation=\"{index}\"]");
                let elements = container
                    .cast::<Element>()
                    .and_then(|c| c.query_selector_all(&selector).ok())
                    .map(|list| {
                        (0..list.length())
                            .filter_map(|i| list.item(i)?.dyn_into::<Element>().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                *drag.borrow_mut() = Some(Drag::Annotation {
                    sx,
                    sy,
                    index,
                    elements,
                    offset: (0.0, 0.0),
                    moved: false,
                });
                return;
            }
            let grabbed = on_edit
                .as_ref()
                .and_then(|_| block_group(e.target())?.get_attribute("data-sid"));
            if let Some(id) = grabbed {
                // Grabbing a selected block moves the whole selection.
                let ids = if selection.blocks().contains(&id) {
                    selection.blocks().to_vec()
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
        let (model, shown) = (props.model.clone(), (*path).clone());
        let (drag_lines, guides, theme) = (drag_lines.clone(), guides.clone(), *theme);
        let (hover, hover_line) = (hover.clone(), hover_line.clone());
        Callback::from(move |e: MouseEvent| {
            let (cx, cy) = (e.client_x() as f64, e.client_y() as f64);
            // The hover card follows the block under a resting pointer, and
            // goes while dragging; so does the highlight of a line's tree.
            let resting = drag
                .borrow()
                .as_ref()
                .is_none_or(|d| matches!(d, Drag::Ended { .. }));
            let over = resting
                .then(|| block_group(e.target())?.get_attribute("data-sid"))
                .flatten();
            let line = resting
                .then(|| {
                    let hit = closest(e.target(), "polyline.wire-hit, polyline.trunk-hit")?;
                    let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
                    let sys = model.system_at(&refs)?;
                    match endpoint_of(&hit, "src-") {
                        Some(src) => sys.lines.iter().position(|l| l.src.as_ref() == Some(&src)),
                        None => {
                            let dst = endpoint_of(&hit, "dst-")?;
                            sys.lines
                                .iter()
                                .position(|l| unlinked_model::edit::drives(l, &dst))
                        }
                    }
                })
                .flatten();
            if line != *hover_line {
                hover_line.set(line);
            }
            if over.as_ref() != hover.as_ref().map(|(id, ..)| id) {
                let r = container
                    .cast::<HtmlElement>()
                    .map(|el| el.get_bounding_client_rect());
                // Below right of the pointer, kept inside the canvas at
                // roughly the card's largest size.
                hover.set(over.zip(r).map(|(id, r)| {
                    let x = (cx - r.left() + 14.0).min(r.width() - 300.0).max(0.0);
                    let y = (cy - r.top() + 18.0).min(r.height() - 130.0).max(0.0);
                    (id, x, y)
                }));
            }
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
                // The end snaps to the nearest port it can connect to, or
                // over a block to that block's nearest free one.
                Some(Drag::Wire { from, target }) => {
                    let to = (*rendered)
                        .as_ref()
                        .ok()
                        .and_then(|svg| to_diagram(&e, &container, &view, svg));
                    let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
                    let (Some(start), Some(to), Some(sys)) =
                        (*wire_preview, to, model.system_at(&refs))
                    else {
                        return;
                    };
                    let over = block_group(e.target())
                        .and_then(|g| g.get_attribute("data-sid"))
                        .map(BlockId);
                    let snapped = snap_port(sys, from, to, SNAP_RADIUS / view.scale, over.as_ref());
                    let end = snapped.as_ref().map_or(to, |(_, p)| *p);
                    *target = snapped.map(|(ep, _)| ep);
                    wire_preview.set(Some((start.0, end, target.is_some())));
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
                        let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
                        let sys = model.system_at(&refs);
                        // Smart guides line blocks up, unless Alt is held.
                        let (to, lined_up) = match sys.filter(|_| !e.alt_key()) {
                            Some(sys) => {
                                let ids: Vec<BlockId> = blocks
                                    .iter()
                                    .map(|(_, id, _)| BlockId(id.clone()))
                                    .collect();
                                align(sys, &ids, (snap(dx), snap(dy)), GUIDE_REACH / view.scale)
                            }
                            None => ((snap(dx), snap(dy)), Vec::new()),
                        };
                        // Even where the offset stays: Alt can drop guides.
                        if *guides != lined_up {
                            guides.set(lined_up);
                        }
                        if to == *offset {
                            return;
                        }
                        *offset = to;
                        for (group, ..) in blocks.iter() {
                            let _ = group.set_attribute(
                                "transform",
                                &format!("translate({} {})", offset.0, offset.1),
                            );
                        }
                        // Their lines follow, routed as the move will route
                        // them.
                        if let Some(sys) = sys {
                            let moves: Vec<(BlockId, Rect)> = blocks
                                .iter()
                                .map(|(_, id, r)| {
                                    let (dx, dy) = to;
                                    let moved = Rect::new(
                                        r.left + dx,
                                        r.top + dy,
                                        r.right + dx,
                                        r.bottom + dy,
                                    );
                                    (BlockId(id.clone()), moved)
                                })
                                .collect();
                            let (moved, touched) = unlinked_model::edit::preview_moves(sys, &moves);
                            let opts = RenderOptions {
                                theme,
                                ..Default::default()
                            };
                            let svg = unlinked_render::render_lines_svg(&moved, &touched, &opts);
                            drag_lines.set(Some((touched, svg)));
                        }
                    }
                }
                Some(Drag::Annotation {
                    sx,
                    sy,
                    elements,
                    offset,
                    moved,
                    ..
                }) => {
                    let (dx, dy) = (cx - *sx, cy - *sy);
                    *moved |= dx.abs() + dy.abs() > 3.0;
                    if *moved {
                        let snap = |v: f64| (v / view.scale / SNAP).round() * SNAP;
                        *offset = (snap(dx), snap(dy));
                        for element in elements.iter() {
                            let _ = element.set_attribute(
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
                    target,
                    points,
                    fixed,
                    segment,
                    start,
                    route,
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
                    let end = target.fixed_end();
                    *route = (delta != 0.0)
                        .then(|| geometry::drag_segment(points, *fixed, end, *segment, delta))
                        .flatten()
                        .map(|mut r| {
                            // A trunk's stored vertices end at its junction.
                            if let RouteTarget::Trunk(_) = target {
                                r.extend(points.last());
                            }
                            r
                        });
                    // Preview as drawn: the fixed start, then the new
                    // vertices, then a leaf's port end.
                    route_preview.set(route.as_ref().map(|r| {
                        let mut drawn = points[..*fixed].to_vec();
                        drawn.extend(r);
                        if let RouteTarget::Leaf(_) = target {
                            drawn.extend(&points[points.len() - end..]);
                        }
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
        let (selection, view) = (selection.clone(), view.clone());
        let (model, on_error) = (props.model.clone(), props.on_error.clone());
        let (drag_lines, guides) = (drag_lines.clone(), guides.clone());
        let blocks: Vec<(String, Rect)> = system
            .map(|s| {
                s.blocks
                    .iter()
                    .map(|b| (b.id.0.clone(), b.position))
                    .collect()
            })
            .unwrap_or_default();
        let annotations: Vec<Annotation> =
            system.map(|s| s.annotations.clone()).unwrap_or_default();
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
                Some(Drag::Wire { from, target }) => {
                    wire_preview.set(None);
                    let to = target.or_else(|| {
                        closest(e.target(), "circle.port-hit").and_then(|c| endpoint_of(&c, ""))
                    });
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
                Some(Drag::Segment { target, route, .. }) => {
                    route_preview.set(None);
                    let moved = route.is_some();
                    if let (Some(points), Some(on_edit)) = (route, &on_edit) {
                        let system = system_ref.clone();
                        on_edit.emit(match target {
                            RouteTarget::Leaf(dst) => Edit::SetRoute {
                                system,
                                dst,
                                points,
                            },
                            RouteTarget::Trunk(src) => Edit::SetTrunkRoute {
                                system,
                                src,
                                points,
                            },
                        });
                    }
                    moved
                }
                Some(Drag::Annotation {
                    index,
                    offset,
                    moved,
                    ..
                }) => {
                    let current = annotations.get(index);
                    if let (true, Some(on_edit), Some(a)) =
                        (moved && offset != (0.0, 0.0), &on_edit, current)
                    {
                        let (dx, dy) = offset;
                        let p = a.position;
                        let position =
                            Rect::new(p.left + dx, p.top + dy, p.right + dx, p.bottom + dy);
                        on_edit.emit(Edit::MoveAnnotation {
                            system: system_ref.clone(),
                            target: AnnotationTarget {
                                index,
                                expected: a.clone(),
                            },
                            position,
                        });
                        // A selected annotation stays selected where it went.
                        if matches!(&*selection, Selection::Annotation(i, s) if *i == index && s == a)
                        {
                            let mut moved = a.clone();
                            moved.position = position;
                            selection.set(Selection::Annotation(index, moved));
                        }
                    }
                    moved
                }
                Some(Drag::Blocks {
                    blocks,
                    offset,
                    moved,
                    ..
                }) => {
                    drag_lines.set(None);
                    guides.set(Vec::new());
                    let copy = e.ctrl_key() || e.meta_key();
                    if let (true, true, Some(on_edits)) =
                        (moved && offset != (0.0, 0.0), copy, &on_edits)
                    {
                        // Ctrl/Cmd held on release copies the blocks to
                        // where they were dragged and leaves the originals,
                        // as in Simulink.
                        for (group, ..) in &blocks {
                            let _ = group.remove_attribute("transform");
                        }
                        let ids: Vec<BlockId> = blocks
                            .iter()
                            .map(|(_, id, _)| BlockId(id.clone()))
                            .collect();
                        let copied = unlinked_model::edit::next_sid(&model).and_then(|sid| {
                            let at = Point::new(offset.0, offset.1);
                            unlinked_model::edit::duplicate(&model, &system_ref, &ids, at, sid).ok()
                        });
                        match copied {
                            Some(group) => {
                                let added = group
                                    .iter()
                                    .filter_map(|edit| match edit {
                                        Edit::AddBlock { id, .. } => Some(id.0.clone()),
                                        _ => None,
                                    })
                                    .collect();
                                on_edits.emit(group);
                                selection.set(Selection::Blocks(added));
                            }
                            None => {
                                if let Some(on_error) = &on_error {
                                    on_error.emit(
                                        "Cannot copy these blocks: only native blocks copy".into(),
                                    );
                                }
                            }
                        }
                    } else if let (true, Some(on_edits)) =
                        (moved && offset != (0.0, 0.0), &on_edits)
                    {
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
                            selection.blocks().to_vec()
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
                        selection.set(Selection::Blocks(ids));
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
        let selection = selection.clone();
        let annotations: Vec<Annotation> =
            system.map(|s| s.annotations.clone()).unwrap_or_default();
        let (on_edit, on_error, system_ref) =
            (on_edit.clone(), props.on_error.clone(), system_ref.clone());
        let (model, shown) = (props.model.clone(), (*path).clone());
        Callback::from(move |e: MouseEvent| {
            let moved = matches!(drag.borrow_mut().take(), Some(Drag::Ended { moved: true }));
            if moved {
                return;
            }
            let clicked = hit(e.target(), &annotations);
            // Ctrl/Cmd-clicking a block with another selected connects them,
            // as in Simulink: the selected block's first free output to the
            // clicked block's first free input.
            if let (true, Some(on_edit), [from], Selection::Blocks(to)) = (
                e.ctrl_key() || e.meta_key(),
                &on_edit,
                selection.blocks(),
                &clicked,
            ) {
                let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
                let pair = to.first().filter(|to| *to != from).and_then(|to| {
                    auto_connect(
                        model.system_at(&refs)?,
                        &BlockId(from.clone()),
                        &BlockId(to.clone()),
                    )
                });
                if let Some((src, dst)) = pair {
                    on_edit.emit(Edit::Connect {
                        system: system_ref.clone(),
                        src,
                        dst,
                    });
                    selection.set(clicked);
                } else if let Some(on_error) = &on_error {
                    on_error.emit("No free output and input to connect these blocks".into());
                }
                return;
            }
            selection.set(match clicked {
                // Shift-click adds or removes a block.
                Selection::Blocks(ids) if e.shift_key() => {
                    let mut selected = selection.blocks().to_vec();
                    for id in ids {
                        match selected.iter().position(|s| *s == id) {
                            Some(i) => {
                                selected.remove(i);
                            }
                            None => selected.push(id),
                        }
                    }
                    Selection::Blocks(selected)
                }
                other => other,
            });
        })
    };
    let lone_subsystem = matches!(
        selection.blocks(),
        [id] if system
            .and_then(|s| s.block(&BlockId(id.clone())))
            .is_some_and(|b| b.subsystem.is_some())
    );
    // Editing actions, from the keyboard, the toolbar or the context menu.
    let act = {
        let (selection, on_edit, on_edits, system_ref) = (
            selection.clone(),
            on_edit.clone(),
            props.on_edit.clone(),
            system_ref.clone(),
        );
        let lines: Vec<Line> = system.map(|s| s.lines.clone()).unwrap_or_default();
        let blocks: Vec<Block> = system.map(|s| s.blocks.clone()).unwrap_or_default();
        let annotations: Vec<Annotation> =
            system.map(|s| s.annotations.clone()).unwrap_or_default();
        let (clipboard, model, on_error, select_created) = (
            clipboard.clone(),
            props.model.clone(),
            props.on_error.clone(),
            select_created.clone(),
        );
        let (container, refresh) = (container.clone(), refresh.clone());
        Callback::from(move |action: Action| {
            let (Some(on_edit), Some(on_edits)) = (&on_edit, &on_edits) else {
                return;
            };
            // Keep keyboard focus on the diagram after a button click.
            if let Some(el) = container.cast::<HtmlElement>() {
                let _ = el.focus();
            }
            if !available(
                action,
                &selection,
                clipboard.borrow().is_some(),
                lone_subsystem,
            ) {
                return;
            }
            let chosen: Vec<&Block> = blocks
                .iter()
                .filter(|b| selection.blocks().contains(&b.id.0))
                .collect();
            match action {
                // Copy, then delete what was copied; only blocks that can
                // be pasted back are cut, and then nothing else changes.
                Action::Cut => {
                    let ids: Vec<BlockId> =
                        selection.blocks().iter().cloned().map(BlockId).collect();
                    if let Err(e) = unlinked_model::clipboard::paste(
                        &model,
                        &system_ref,
                        &ids,
                        0,
                        &model,
                        &system_ref,
                    ) {
                        if let Some(on_error) = &on_error {
                            on_error.emit(format!("Cannot cut: {e}"));
                        }
                        return;
                    }
                    if let Some(group) = confirm_delete(&model, &system_ref, &ids, &lines) {
                        *clipboard.borrow_mut() = Some(Clipboard {
                            model: model.clone(),
                            system: system_ref.clone(),
                            ids,
                            pastes: Default::default(),
                        });
                        on_edits.emit(group);
                        selection.set(Selection::Nothing);
                    }
                }
                // Paste a copy of the selection beside it, leaving the
                // clipboard alone.
                Action::Duplicate => {
                    let ids: Vec<BlockId> =
                        selection.blocks().iter().cloned().map(BlockId).collect();
                    match unlinked_model::clipboard::paste(
                        &model,
                        &system_ref,
                        &ids,
                        0,
                        &model,
                        &system_ref,
                    ) {
                        Ok(group) => {
                            let added = group
                                .iter()
                                .filter_map(|edit| match edit {
                                    Edit::AddBlock { id, .. } => Some(id.0.clone()),
                                    _ => None,
                                })
                                .collect();
                            on_edits.emit(group);
                            selection.set(Selection::Blocks(added));
                        }
                        Err(e) => {
                            if let Some(on_error) = &on_error {
                                on_error.emit(format!("Cannot duplicate: {e}"));
                            }
                        }
                    }
                }
                // The inspector shows the lone selected block's name.
                Action::Rename => {
                    let field = container
                        .cast::<Element>()
                        .and_then(|c| c.parent_element())
                        .and_then(|v| {
                            v.query_selector(".inspector input.block-name")
                                .ok()
                                .flatten()
                        })
                        .and_then(|i| i.dyn_into::<HtmlInputElement>().ok());
                    if let Some(field) = field {
                        let _ = field.focus();
                        field.select();
                    }
                }
                // Copy the selected blocks; paste duplicates them with their
                // internal lines, offset from the originals.
                Action::Copy => {
                    if selection.blocks().is_empty() {
                        return;
                    }
                    *clipboard.borrow_mut() = Some(Clipboard {
                        model: model.clone(),
                        system: system_ref.clone(),
                        ids: selection.blocks().iter().cloned().map(BlockId).collect(),
                        pastes: Default::default(),
                    });
                    // Paste becomes available.
                    refresh.force_update();
                }
                Action::Paste => {
                    let mut clip = clipboard.borrow_mut();
                    let Some(clip) = clip.as_mut() else {
                        return;
                    };
                    // Into the shown system, whichever it was copied from.
                    let nth = clip.pastes.get(&system_ref).copied().unwrap_or(0);
                    let pasted = unlinked_model::clipboard::paste(
                        &clip.model,
                        &clip.system,
                        &clip.ids,
                        nth,
                        &model,
                        &system_ref,
                    )
                    .map_err(|e| e.to_string());
                    match pasted {
                        Ok(group) => {
                            clip.pastes
                                .insert(system_ref.clone(), nth.saturating_add(1));
                            let added = group
                                .iter()
                                .filter_map(|edit| match edit {
                                    Edit::AddBlock { id, .. } => Some(id.0.clone()),
                                    _ => None,
                                })
                                .collect();
                            on_edits.emit(group);
                            selection.set(Selection::Blocks(added));
                        }
                        Err(message) => {
                            if let Some(on_error) = &on_error {
                                on_error.emit(format!("Cannot paste: {message}"));
                            }
                        }
                    }
                }
                Action::Rotate | Action::Flip => {
                    // Each selected block turns or flips in place, as one
                    // action.
                    let mut group = Vec::new();
                    for b in &chosen {
                        let (orientation, mirrored) = if action == Action::Rotate {
                            geometry::rotated(b.orientation, b.mirrored)
                        } else {
                            geometry::flipped(b.orientation, b.mirrored)
                        };
                        if action == Action::Rotate {
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
                    if !group.is_empty() {
                        on_edits.emit(group);
                    }
                }
                // Move the selected blocks by (dx, dy), as one action.
                Action::Nudge(dx, dy) => {
                    let group: Vec<Edit> = chosen
                        .iter()
                        .map(|b| {
                            let p = b.position;
                            Edit::MoveBlock {
                                system: system_ref.clone(),
                                id: b.id.clone(),
                                position: Rect::new(
                                    p.left + dx,
                                    p.top + dy,
                                    p.right + dx,
                                    p.bottom + dy,
                                ),
                            }
                        })
                        .collect();
                    if !group.is_empty() {
                        on_edits.emit(group);
                    }
                }
                Action::SelectAll => selection.set(Selection::Blocks(
                    blocks.iter().map(|b| b.id.0.clone()).collect(),
                )),
                // In selection order, so the first selected leads.
                Action::Arrange(how) => {
                    let placed: Vec<(BlockId, Rect)> = selection
                        .blocks()
                        .iter()
                        .filter_map(|id| {
                            let b = blocks.iter().find(|b| b.id.0 == *id)?;
                            Some((b.id.clone(), b.position))
                        })
                        .collect();
                    let group: Vec<Edit> = arranged(&placed, how)
                        .into_iter()
                        .map(|(id, position)| Edit::MoveBlock {
                            system: system_ref.clone(),
                            id,
                            position,
                        })
                        .collect();
                    if !group.is_empty() {
                        on_edits.emit(group);
                    }
                }
                Action::AutoRoute => {
                    let system = system_ref.clone();
                    on_edit.emit(match &*selection {
                        Selection::Wire(dst) => Edit::SetRoute {
                            system,
                            dst: dst.clone(),
                            points: Vec::new(),
                        },
                        Selection::Trunk(src) => Edit::SetTrunkRoute {
                            system,
                            src: src.clone(),
                            points: Vec::new(),
                        },
                        _ => return,
                    });
                }
                // Flatten a lone selected subsystem into its parent.
                Action::Expand => {
                    if let [id] = selection.blocks() {
                        on_edit.emit(Edit::ExpandSubsystem {
                            system: system_ref.clone(),
                            id: BlockId(id.clone()),
                        });
                        selection.set(Selection::Nothing);
                    }
                }
                // Group the selected blocks into a new subsystem, named as
                // Simulink names one, and select it.
                Action::Group => {
                    if selection.blocks().is_empty() {
                        return;
                    }
                    let Some(sid) = unlinked_model::edit::next_sid(&model) else {
                        return;
                    };
                    let names: std::collections::HashSet<&str> =
                        blocks.iter().map(|b| b.name.as_str()).collect();
                    let name = std::iter::once("Subsystem".to_string())
                        .chain((1..).map(|i| format!("Subsystem{i}")))
                        .find(|n| !names.contains(n.as_str()))
                        .expect("some suffix is free");
                    let id = BlockId(sid.to_string());
                    *select_created.borrow_mut() = Some(id.clone());
                    on_edit.emit(Edit::CreateSubsystem {
                        system: system_ref.clone(),
                        ids: selection.blocks().iter().cloned().map(BlockId).collect(),
                        id,
                        name,
                    });
                }
                Action::Delete => match &*selection {
                    Selection::Wire(dst) => {
                        on_edit.emit(Edit::Disconnect {
                            system: system_ref.clone(),
                            dst: dst.clone(),
                        });
                        selection.set(Selection::Nothing);
                    }
                    // Only the annotation that was selected, if it is still
                    // where it was.
                    Selection::Annotation(index, expected) => {
                        if annotations.get(*index) == Some(expected) {
                            on_edit.emit(Edit::DeleteAnnotation {
                                system: system_ref.clone(),
                                target: AnnotationTarget {
                                    index: *index,
                                    expected: expected.clone(),
                                },
                            });
                        }
                        selection.set(Selection::Nothing);
                    }
                    Selection::Blocks(ids) if !ids.is_empty() => {
                        let ids: Vec<BlockId> = ids.iter().cloned().map(BlockId).collect();
                        if let Some(group) = confirm_delete(&model, &system_ref, &ids, &lines) {
                            on_edits.emit(group);
                            selection.set(Selection::Nothing);
                        }
                    }
                    _ => {}
                },
            }
        })
    };
    let edit_keydown = {
        let (act, selection, clipboard) = (act.clone(), selection.clone(), clipboard.clone());
        let (editable, subsystem) = (props.on_edit.is_some(), lone_subsystem);
        Callback::from(move |e: KeyboardEvent| {
            let command = e.ctrl_key() || e.meta_key();
            if !editable || e.alt_key() {
                return;
            }
            let key = e.key().to_ascii_lowercase();
            let action = match key.as_str() {
                "x" if command => Action::Cut,
                "c" if command => Action::Copy,
                "v" if command => Action::Paste,
                "d" if command => Action::Duplicate,
                "f2" => Action::Rename,
                "r" if command => Action::Rotate,
                "i" if command => Action::Flip,
                "a" if command => Action::SelectAll,
                "g" if command && e.shift_key() => Action::Expand,
                "g" if command => Action::Group,
                "delete" | "backspace" => Action::Delete,
                // Arrows nudge the selection by a grid step, or one unit
                // with Shift.
                "arrowleft" | "arrowright" | "arrowup" | "arrowdown" if !command => {
                    let step = if e.shift_key() { 1.0 } else { SNAP };
                    match key.as_str() {
                        "arrowleft" => Action::Nudge(-step, 0.0),
                        "arrowright" => Action::Nudge(step, 0.0),
                        "arrowup" => Action::Nudge(0.0, -step),
                        _ => Action::Nudge(0.0, step),
                    }
                }
                _ => return,
            };
            // Keys are only taken from the browser when they act on something.
            if available(action, &selection, clipboard.borrow().is_some(), subsystem) {
                e.prevent_default();
                act.emit(action);
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
        let (on_edit, system_ref, model, select_created) = (
            on_edit.clone(),
            system_ref.clone(),
            props.model.clone(),
            select_created.clone(),
        );
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
                // Selected once added, so it can be wired or set up at once.
                if let Edit::AddBlock { id, .. } = &edit {
                    *select_created.borrow_mut() = Some(id.clone());
                }
                on_edit.emit(edit);
            }
        })
    };
    // The context menu, open at a client position.
    let oncontextmenu = {
        let (menu, selection, editable) =
            (menu.clone(), selection.clone(), props.on_edit.is_some());
        let annotations: Vec<Annotation> =
            system.map(|s| s.annotations.clone()).unwrap_or_default();
        Callback::from(move |e: MouseEvent| {
            if !editable {
                return;
            }
            e.prevent_default();
            // The menu acts on what was right-clicked, as a click selects
            // it, except that a block of the selection keeps the selection.
            match hit(e.target(), &annotations) {
                Selection::Blocks(ids) if ids.iter().all(|id| selection.blocks().contains(id)) => {}
                other => selection.set(other),
            }
            // Placed where it is drawn, once its items are known.
            menu.set(Some((e.client_x(), e.client_y())));
        })
    };
    // Add a block, or an annotation selected so its text can be typed in
    // the inspector, at a diagram point.
    let add_at = {
        let (selection, select_created, model) = (
            selection.clone(),
            select_created.clone(),
            props.model.clone(),
        );
        let (on_edit, system_ref) = (on_edit.clone(), system_ref.clone());
        let annotation_count = system.map_or(0, |s| s.annotations.len());
        Callback::from(move |(pick, at): (Pick, Point)| {
            let Some(on_edit) = &on_edit else {
                return;
            };
            match pick {
                Pick::Block(type_key) => {
                    if let Some(edit) = add_block_at(&model, &system_ref, type_key, at) {
                        if let Edit::AddBlock { id, .. } = &edit {
                            *select_created.borrow_mut() = Some(id.clone());
                        }
                        on_edit.emit(edit);
                    }
                }
                Pick::Annotation => {
                    let Some(sid) = unlinked_model::edit::next_sid(&model) else {
                        return;
                    };
                    let snap = |v: f64| (v / SNAP).round() * SNAP;
                    let (x, y) = (snap(at.x), snap(at.y));
                    let (text, position) = ("Annotation".to_string(), Rect::new(x, y, x, y));
                    on_edit.emit(Edit::AddAnnotation {
                        system: system_ref.clone(),
                        id: sid.to_string(),
                        text: text.clone(),
                        position,
                    });
                    // As AddAnnotation records it.
                    let added = Annotation {
                        text,
                        position,
                        rich_text: false,
                        properties: [("SID".to_string(), sid.to_string())].into(),
                    };
                    selection.set(Selection::Annotation(annotation_count, added));
                }
            }
        })
    };
    let ondblclick = {
        let path = path.clone();
        let selection = selection.clone();
        let model = props.model.clone();
        let (container, rendered, view) = (container.clone(), rendered.clone(), view.clone());
        let (quick, editable) = (quick.clone(), props.on_edit.is_some());
        Callback::from(move |e: MouseEvent| {
            // While editing, double-clicking empty canvas opens the quick
            // insert there.
            let on_something = closest(
                e.target(),
                "g.block, [data-annotation], polyline, circle, rect.resize-handle, g.state",
            )
            .is_some();
            if editable && !on_something {
                let at = (*rendered)
                    .as_ref()
                    .ok()
                    .and_then(|svg| to_diagram(&e, &container, &view, svg));
                let origin = container
                    .cast::<HtmlElement>()
                    .map(|el| el.get_bounding_client_rect());
                if let (Some(at), Some(r)) = (at, origin) {
                    // Kept inside the canvas, at roughly its largest size.
                    let x = (e.client_x() as f64 - r.left()).min(r.width() - 330.0);
                    let y = (e.client_y() as f64 - r.top()).min(r.height() - 420.0);
                    quick.set(Some((x.max(0.0), y.max(0.0), at)));
                }
                return;
            }
            // Double-clicking a line names its signal, as in Simulink: the
            // first click selected it, so its inspector is showing.
            let wire = closest(e.target(), "polyline.wire-hit, polyline.trunk-hit").is_some();
            if editable && wire {
                let field = container
                    .cast::<Element>()
                    .and_then(|c| c.parent_element())
                    .and_then(|v| {
                        v.query_selector(".inspector input.signal-name")
                            .ok()
                            .flatten()
                    })
                    .and_then(|i| i.dyn_into::<HtmlInputElement>().ok());
                if let Some(field) = field {
                    let _ = field.focus();
                    field.select();
                }
                return;
            }
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
                selection.set(Selection::Nothing);
                return;
            }
            // Double-clicking another block edits its parameters, and an
            // annotation its text, as in Simulink: the first click selected
            // it, so its inspector is showing.
            let field = if closest(e.target(), "[data-annotation]").is_some() {
                ".inspector textarea.annotation-text"
            } else if block_group(e.target()).is_some() {
                ".inspector :is(input.param, select, textarea)"
            } else {
                return;
            };
            let field = container
                .cast::<Element>()
                .and_then(|c| c.parent_element())
                .and_then(|v| v.query_selector(field).ok().flatten())
                .and_then(|f| f.dyn_into::<HtmlElement>().ok());
            if let (true, Some(field)) = (editable, field) {
                let _ = field.focus();
                if let Some(input) = field.dyn_ref::<HtmlInputElement>() {
                    input.select();
                }
            }
        })
    };

    // The inspector and resize handles follow a lone selected block.
    let selected_block = system.and_then(|s| match selection.blocks() {
        [sid] => s.blocks.iter().find(|b| &b.id.0 == sid),
        _ => None,
    });
    // A line selected by the input it drives or the source of its trunk;
    // ends shared by several lines (in imported models) select none.
    let selected_line = system.and_then(|s| match &*selection {
        Selection::Wire(dst) => unlinked_model::edit::line_into(s, dst),
        Selection::Trunk(src) => unlinked_model::edit::line_from(s, src),
        _ => None,
    });
    let selected_annotation = system.and_then(|s| selection.annotation(s));
    let selected_chart = selected_block.and_then(|b| {
        let mut p = refs.clone();
        p.push(&b.name);
        props.model.chart_at(&p)
    });

    let open_path = {
        let path = path.clone();
        move |p: Vec<String>| {
            let path = path.clone();
            Callback::from(move |_: MouseEvent| path.set(p.clone()))
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
        Callback::from(move |()| {
            if let (Some(el), Ok(svg)) = (container.cast::<HtmlElement>(), rendered.as_ref()) {
                if let Some(size) = svg_size(svg) {
                    view.dispatch(ViewAction::Set(fit(&el, size)));
                }
            }
        })
    };
    // Bring block `id` of the shown system into view, at the current zoom,
    // when it is not wholly in view already.
    let reveal = {
        let (view, container, rendered) = (view.clone(), container.clone(), rendered.clone());
        let blocks: Vec<(String, Rect)> = system
            .map(|s| {
                s.blocks
                    .iter()
                    .map(|b| (b.id.0.clone(), b.position))
                    .collect()
            })
            .unwrap_or_default();
        Callback::from(move |id: String| {
            let Some(r) = blocks.iter().find(|(b, _)| *b == id).map(|(_, r)| *r) else {
                return;
            };
            let el = container.cast::<HtmlElement>();
            let vb = (*rendered).as_ref().ok().and_then(|svg| view_box(svg));
            let (Some(el), Some([vx, vy, ..])) = (el, vb) else {
                return;
            };
            let (cw, ch) = (el.client_width() as f64, el.client_height() as f64);
            let v = *view;
            let screen = |x: f64, y: f64| ((x - vx) * v.scale + v.x, (y - vy) * v.scale + v.y);
            let (l, t) = screen(r.left, r.top);
            let (rr, b) = screen(r.right, r.bottom);
            if l >= 0.0 && t >= 0.0 && rr <= cw && b <= ch {
                return;
            }
            let (cx, cy) = ((r.left + r.right) / 2.0 - vx, (r.top + r.bottom) / 2.0 - vy);
            view.dispatch(ViewAction::Set(View {
                scale: v.scale,
                x: cw / 2.0 - cx * v.scale,
                y: ch / 2.0 - cy * v.scale,
            }));
        })
    };
    // Zoom by `factor` about the middle of the canvas.
    let zoom_by = {
        let (view, container) = (view.clone(), container.clone());
        Callback::from(move |factor: f64| {
            if let Some(el) = container.cast::<HtmlElement>() {
                view.dispatch(ViewAction::Zoom {
                    factor,
                    cx: el.client_width() as f64 / 2.0,
                    cy: el.client_height() as f64 / 2.0,
                });
            }
        })
    };

    let enabled =
        |a: Action| available(a, &selection, clipboard.borrow().is_some(), lone_subsystem);
    let line_end = |class: &str, prefix: &str, ep: &Endpoint| {
        format!(
            ".diagram polyline.{class}[data-{prefix}sid=\"{}\"][data-{prefix}kind=\"{}\"][data-{prefix}index=\"{}\"] {{ stroke: rgba(255, 158, 100, 0.55); }}",
            css_string(&ep.block.0),
            ep.port.kind.token(),
            ep.port.index
        )
    };
    let highlight: String = match &*selection {
        Selection::Nothing => String::new(),
        Selection::Blocks(ids) => ids
            .iter()
            .map(|sid| {
                format!(
                    ".diagram g.block[data-sid=\"{}\"] > :is(rect, polygon, ellipse) {{ stroke: #ff9e64; stroke-width: 3px; }}\n",
                    css_string(sid)
                )
            })
            .collect(),
        Selection::Wire(dst) => line_end("wire-hit", "dst-", dst),
        Selection::Trunk(src) => line_end("trunk-hit", "src-", src),
        Selection::Annotation(..) => selected_annotation
            .map(|(i, _)| format!(".diagram .annotation[data-annotation=\"{i}\"] {{ fill: #ff9e64; }}"))
            .unwrap_or_default(),
    };
    // A hovered line lights up with all its branches.
    let highlight = match *hover_line {
        Some(i) => format!(
            ".diagram g[data-line=\"{i}\"] polyline {{ stroke: #7dcfff; stroke-width: 2.5px; }}\n.diagram g[data-line=\"{i}\"] polygon {{ fill: #7dcfff; }}\n"
        ) + &highlight,
        None => highlight,
    };
    // Lines being previewed moved are hidden where they were.
    let highlight = match &*drag_lines {
        Some((touched, _)) => {
            let hidden: String = touched
                .iter()
                .map(|i| format!(".diagram g[data-line=\"{i}\"] {{ visibility: hidden; }}\n"))
                .collect();
            hidden + &highlight
        }
        None => highlight,
    };
    let highlight = match &props.diff {
        Some(d) => diff_css(d, &path, system) + &highlight,
        None => highlight,
    };
    // Inside a chart there are no blocks to outline.
    let highlight = match system.and(system_ids(&props.model, &path)) {
        Some(ids) => problems_css(&props.problems, &ids) + &highlight,
        None => highlight,
    };
    let block_problems: Vec<Diagnostic> = selected_block
        .map(|b| {
            props
                .problems
                .iter()
                .filter(|d| {
                    matches!(&d.target, DiagnosticTarget::Block { system, id, .. }
                        if *system == system_ref && *id == b.id)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let errors = props
        .problems
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = props.problems.len() - errors;
    let toggle_problems = {
        let show_problems = show_problems.clone();
        Callback::from(move |_: MouseEvent| show_problems.set(!*show_problems))
    };
    // Show where a problem is: open its system and select its block.
    let go_to = |d: &Diagnostic| {
        let (path, selection, model, on_settings, select_on_open) = (
            path.clone(),
            selection.clone(),
            props.model.clone(),
            props.on_settings.clone(),
            select_on_open.clone(),
        );
        let reveal = reveal.clone();
        let target = d.target.clone();
        Callback::from(move |_: MouseEvent| match &target {
            DiagnosticTarget::Model => {}
            DiagnosticTarget::Config { .. } => {
                if let Some(on_settings) = &on_settings {
                    on_settings.emit(());
                }
            }
            DiagnosticTarget::Block { system, id, .. } => {
                if let Some(names) = system_names(&model, system) {
                    // Another level selects the block once it is shown.
                    if names == *path {
                        selection.set(Selection::Blocks(vec![id.0.clone()]));
                        reveal.emit(id.0.clone());
                    } else {
                        *select_on_open.borrow_mut() = Some(id.0.clone());
                        path.set(names);
                    }
                }
            }
            DiagnosticTarget::Line { system, .. } => {
                if let Some(names) = system_names(&model, system) {
                    path.set(names);
                    selection.set(Selection::Nothing);
                }
            }
        })
    };
    let problem_list = (*show_problems).then(|| {
        html! {
            <div class="problems-panel">
                <div class="compile-check">
                    <button onclick={props.on_compile.clone().map(|c| c.reform(|_: MouseEvent| ()))}
                        disabled={props.on_compile.is_none()}
                        title="Compile the model for the simulator, off the page's main thread, without simulating">
                        { "Check compile" }
                    </button>
                    <span class="muted">{ &props.compile_status }</span>
                </div>
                if props.problems.is_empty() {
                    <div class="muted">{ "No problems found." }</div>
                }
                { for props.problems.iter().map(|d| html! {
                    <button class={classes!("problem-row", problem_class(d))} onclick={go_to(d)}>
                        <span class="problem-place">{ problem_place(&props.model, &d.target) }</span>
                        <span>{ &d.message }</span>
                    </button>
                }) }
                if props.problems_truncated {
                    <div class="problem warning">{ "The check stopped at its limits: there may be more problems than listed." }</div>
                }
                <div class="muted">{ "Checks structure, settings and simulator support. No simulation is run." }</div>
            </div>
        }
    });

    let canvas = match rendered.as_ref() {
        Ok(svg) => Html::from_html_unchecked(AttrValue::from(svg.clone())),
        Err(e) => html! { <div class="error">{ e }</div> },
    };
    // Drawn over the diagram in its units: the connection being dragged,
    // and the selected block's resize handles and new outline.
    let overlay = match (*rendered).as_ref().ok().and_then(|s| view_box(s)) {
        Some([vx, vy, vw, vh]) => {
            let wire = wire_preview.map(|(a, b, snapped)| {
                let points: Vec<String> = elbow(a, b)
                    .iter()
                    .map(|p| format!("{},{}", p.x, p.y))
                    .collect();
                html! {
                    <>
                        <polyline class={classes!("wire-preview", snapped.then_some("snapped"))}
                            points={points.join(" ")} />
                        if snapped {
                            <circle class="snap-ring" cx={b.x.to_string()} cy={b.y.to_string()}
                                r={(7.0 / view.scale).to_string()} />
                        }
                    </>
                }
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
            // Port value labels, a constant size on screen.
            let labels = (*show_values && !values.is_empty()).then(|| {
                let size = 10.0 / view.scale;
                html! {
                    <g class={classes!("port-values", stale.then_some("stale"))} font-size={size.to_string()}>
                        { for values.iter().map(|(p, text)| html! {
                            <text x={p.x.to_string()} y={p.y.to_string()}>{ text }</text>
                        }) }
                    </g>
                }
            });
            // Guides overhang what they line up a little.
            let overhang = 12.0 / view.scale;
            let lined_up = guides.iter().map(|g| {
                let (x1, y1, x2, y2) = match *g {
                    Guide::Horizontal { y, from, to } => (from - overhang, y, to + overhang, y),
                    Guide::Vertical { x, from, to } => (x, from - overhang, x, to + overhang),
                };
                html! {
                    <line class="guide" x1={x1.to_string()} y1={y1.to_string()}
                        x2={x2.to_string()} y2={y2.to_string()} />
                }
            });
            // Markup parsed as a whole document fragment, so its elements
            // are SVG ones.
            let following = drag_lines.as_ref().map(|(_, lines)| {
                Html::from_html_unchecked(AttrValue::from(format!(
                    "<svg class=\"overlay\" viewBox=\"{vx} {vy} {vw} {vh}\" width=\"{vw}\" height=\"{vh}\"><g fill=\"none\">{lines}</g></svg>"
                )))
            });
            html! {
                <>
                    { for following }
                    <svg class="overlay" viewBox={format!("{vx} {vy} {vw} {vh}")}
                        width={vw.to_string()} height={vh.to_string()}>
                        { for wire }
                        { for handles }
                        { for selecting }
                        { for rerouting }
                        { for lined_up }
                        { for labels }
                    </svg>
                </>
            }
        }
        None => html! {},
    };
    let v = *view;
    let transform = format!(
        "transform: translate({}px, {}px) scale({}); transform-origin: 0 0;",
        v.x, v.y, v.scale
    );
    // While editing, a dot every four grid steps, laid on the diagram's own
    // coordinates so it pans and zooms with it.
    let grid = props
        .on_edit
        .as_ref()
        .and((*rendered).as_ref().ok().and_then(|svg| view_box(svg)))
        .map(|[vx, vy, ..]| {
            let size = SNAP * 4.0 * v.scale;
            format!(
                "background-size: {size}px {size}px; background-position: {}px {}px;",
                v.x - vx * v.scale,
                v.y - vy * v.scale
            )
        });

    let toggle_help = {
        let help = help.clone();
        Callback::from(move |_: MouseEvent| help.set(!*help))
    };
    // Viewing keys work whether or not the diagram is editable; the rest
    // are editing actions.
    let onkeydown = {
        let (fit_view, zoom_by, help) = (fit_view.clone(), zoom_by.clone(), help.clone());
        let (finding, reveal) = (finding.clone(), reveal.clone());
        let (selection, path, menu) = (selection.clone(), path.clone(), menu.clone());
        let order: Vec<String> = system
            .map(|s| s.blocks.iter().map(|b| b.id.0.clone()).collect())
            .unwrap_or_default();
        Callback::from(move |e: KeyboardEvent| {
            // Keys for controls shown over the canvas are theirs.
            let control = closest(
                e.target(),
                "button, input, select, textarea, a, [contenteditable]",
            )
            .is_some();
            if control {
                return;
            }
            let command = e.ctrl_key() || e.meta_key();
            match e.key().as_str() {
                " " if !command => fit_view.emit(()),
                "=" | "+" if command => zoom_by.emit(1.25),
                "-" | "_" if command => zoom_by.emit(1.0 / 1.25),
                "?" => help.set(!*help),
                "f" if command => finding.set(true),
                // With a block selected, Tab steps through the blocks and
                // Shift+Tab back; otherwise Tab moves focus as usual, so the
                // canvas never traps it.
                "Tab" if !command && !selection.blocks().is_empty() => {
                    let n = order.len();
                    let at = selection
                        .blocks()
                        .last()
                        .and_then(|id| order.iter().position(|o| o == id))
                        .unwrap_or(0);
                    let next = if e.shift_key() {
                        (at + n - 1) % n
                    } else {
                        (at + 1) % n
                    };
                    selection.set(Selection::Blocks(vec![order[next].clone()]));
                    reveal.emit(order[next].clone());
                }
                "Escape" => {
                    if menu.is_some() {
                        menu.set(None);
                    } else if *help {
                        help.set(false);
                    } else if *selection != Selection::Nothing {
                        selection.set(Selection::Nothing);
                    } else if !path.is_empty() {
                        let mut p = (*path).clone();
                        p.pop();
                        path.set(p);
                    }
                }
                _ => return edit_keydown.emit(e),
            }
            e.prevent_default();
        })
    };
    html! {
        <div class="viewer">
            <div class="toolbar">
                <button class={classes!("tool", "icon", (*side).then_some("active"))}
                    title="Model browser and library" aria-expanded={side.to_string()}
                    onclick={{
                        let side = side.clone();
                        // Remembered as chosen, not as defaulted.
                        Callback::from(move |_: MouseEvent| {
                            set_pref("side", if *side { "hidden" } else { "shown" });
                            side.set(!*side)
                        })
                    }}>{ icons::sidebar() }</button>
                { props.leading.clone() }
                if props.on_edit.is_some() {
                    { for TOOLBAR.iter().map(|group| html! {
                        <span class="tool-group">
                            { for group.iter().map(|&a| action_button(a, enabled(a), &act, false)) }
                        </span>
                    }) }
                }
                <span class="spacer" />
                <button class={classes!("tool", "problems-button", (errors > 0).then_some("has-errors"),
                    (warnings > 0 && errors == 0).then_some("has-warnings"),
                    (*show_problems).then_some("active"))} onclick={toggle_problems}
                    title={format!("{errors} error{} and {warnings} warning{} found by the static check{}",
                        if errors == 1 { "" } else { "s" }, if warnings == 1 { "" } else { "s" },
                        if props.problems_truncated { " (incomplete)" } else { "" })}>
                    <span class="count error-count">{ errors }</span>
                    <span class="count warning-count">{ warnings }</span>
                    <span>{ "Problems" }</span>
                </button>
                <span class="tool-group zoom-group">
                    <button class="tool icon" title="Zoom out (Ctrl+-)"
                        onclick={zoom_by.reform(|_: MouseEvent| 1.0 / 1.25)}>{ icons::zoom_out() }</button>
                    <span class="zoom" title="Zoom">{ format!("{:.0}%", v.scale * 100.0) }</span>
                    <button class="tool icon" title="Zoom in (Ctrl+=)"
                        onclick={zoom_by.reform(|_: MouseEvent| 1.25)}>{ icons::zoom_in() }</button>
                    <button class="tool icon" title="Fit to view (Space)"
                        onclick={fit_view.reform(|_: MouseEvent| ())}>{ icons::fit() }</button>
                </span>
                <button class="tool icon" onclick={toggle_theme}
                    title={if *theme == Theme::Dark { "Light canvas" } else { "Dark canvas" }}>
                    { if *theme == Theme::Dark { icons::sun() } else { icons::moon() } }
                </button>
                if let Some(run) = &run {
                    <button class={classes!("tool", (*show_values).then_some("active"), stale.then_some("stale"))}
                        aria-pressed={show_values.to_string()}
                        title={format!("Label lines with their values from the v{} run{}", run.version,
                            if stale { " (the diagram has changed since)" } else { "" })}
                        onclick={{
                            let show_values = show_values.clone();
                            Callback::from(move |_: MouseEvent| show_values.set(!*show_values))
                        }}>
                        { icons::probe() }<span>{ "Values" }</span>
                    </button>
                }
                <button class={classes!("tool", "icon", (*finding).then_some("active"))}
                    title="Find blocks (Ctrl+F)" onclick={{
                        let finding = finding.clone();
                        Callback::from(move |_: MouseEvent| finding.set(!*finding))
                    }}>{ icons::search() }</button>
                <button class={classes!("tool", "icon", (*help).then_some("active"))}
                    onclick={toggle_help} title="Keyboard and mouse shortcuts (?)">{ icons::help() }</button>
                { props.trailing.clone() }
            </div>
            <div class="viewer-body">
                if *side {
                    <aside class="tree">
                        <SystemTree model={props.model.clone()} current={(*path).clone()} on_open={Callback::from({
                            let path = path.clone();
                            move |p: Vec<String>| path.set(p)
                        })} />
                        if props.on_edit.is_some() {
                            <BlockPalette on_add={{
                                let (add_at, container, rendered, view) =
                                    (add_at.clone(), container.clone(), rendered.clone(), view.clone());
                                // In the middle of what is shown.
                                Callback::from(move |type_key: &'static str| {
                                    let el = container.cast::<HtmlElement>();
                                    let vb = (*rendered).as_ref().ok().and_then(|svg| view_box(svg));
                                    if let (Some(el), Some([vx, vy, ..])) = (el, vb) {
                                        let at = Point::new(
                                            (el.client_width() as f64 / 2.0 - view.x) / view.scale + vx,
                                            (el.client_height() as f64 / 2.0 - view.y) / view.scale + vy,
                                        );
                                        add_at.emit((Pick::Block(type_key), at));
                                    }
                                })
                            }} />
                        }
                    </aside>
                }
                <div class={classes!("diagram", (*theme == Theme::Light).then_some("light"), props.on_edit.is_some().then_some("editing"))}
                    ref={container.clone()} tabindex="0" style={grid}
                    {onmousedown} {onmousemove} onmouseup={end_drag.clone()} onmouseleave={{
                        let (hover, hover_line, end_drag) = (hover.clone(), hover_line.clone(), end_drag.clone());
                        Callback::from(move |e: MouseEvent| {
                            hover.set(None);
                            hover_line.set(None);
                            end_drag.emit(e)
                        })
                    }}
                    {onclick} {ondblclick} {onkeydown} {ondragover} {ondrop} {oncontextmenu}>
                    <style>{ highlight }</style>
                    <div class="canvas" style={transform}>{ canvas }{ overlay }</div>
                    if let Some((id, x, y)) = &*hover {
                        <HoverCard model={props.model.clone()} path={(*path).clone()}
                            id={BlockId(id.clone())} x={*x} y={*y} />
                    }
                    // Over the canvas, as Simulink's address bar.
                    <nav class="crumbs" onmousedown={Callback::from(|e: MouseEvent| e.stop_propagation())}
                        ondblclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        { for crumbs }
                    </nav>
                    if let Some((x, y, at)) = *quick {
                        <QuickInsert {x} {y}
                            on_pick={{
                                let (add_at, quick, container) = (add_at.clone(), quick.clone(), container.clone());
                                Callback::from(move |pick: Pick| {
                                    quick.set(None);
                                    add_at.emit((pick, at));
                                    if let Some(el) = container.cast::<HtmlElement>() {
                                        let _ = el.focus();
                                    }
                                })
                            }}
                            on_close={{
                                let (quick, container) = (quick.clone(), container.clone());
                                Callback::from(move |()| {
                                    quick.set(None);
                                    if let Some(el) = container.cast::<HtmlElement>() {
                                        let _ = el.focus();
                                    }
                                })
                            }} />
                    }
                    if *finding {
                        <FindBlocks model={props.model.clone()}
                            on_pick={{
                                let reveal = reveal.clone();
                                let (finding, path, selection, select_on_open, container) = (
                                    finding.clone(),
                                    path.clone(),
                                    selection.clone(),
                                    select_on_open.clone(),
                                    container.clone(),
                                );
                                Callback::from(move |f: Found| {
                                    finding.set(false);
                                    // Another level selects the block once shown.
                                    if f.path == *path {
                                        reveal.emit(f.id.clone());
                                        selection.set(Selection::Blocks(vec![f.id]));
                                    } else {
                                        *select_on_open.borrow_mut() = Some(f.id);
                                        path.set(f.path);
                                    }
                                    if let Some(el) = container.cast::<HtmlElement>() {
                                        let _ = el.focus();
                                    }
                                })
                            }}
                            on_close={{
                                let (finding, container) = (finding.clone(), container.clone());
                                Callback::from(move |()| {
                                    finding.set(false);
                                    if let Some(el) = container.cast::<HtmlElement>() {
                                        let _ = el.focus();
                                    }
                                })
                            }} />
                    }
                    if *help {
                        <ShortcutHelp editable={props.on_edit.is_some()} on_close={{
                            let help = help.clone();
                            Callback::from(move |()| help.set(false))
                        }} />
                    }
                </div>
                if let (Some((x, y)), true) = (*menu, props.on_edit.is_some()) {
                    // Clicking anywhere, including an item, closes it.
                    <div class="context-backdrop" onclick={{
                        let menu = menu.clone();
                        Callback::from(move |_: MouseEvent| menu.set(None))
                    }} oncontextmenu={{
                        let menu = menu.clone();
                        Callback::from(move |e: MouseEvent| { e.prevent_default(); menu.set(None) })
                    }}>
                        <div class="context-menu" style={menu_place(x, y, MENU.iter().filter(|&&a| enabled(a)).count())}>
                            { for MENU.iter().filter(|&&a| enabled(a)).map(|&a| action_button(a, true, &act, true)) }
                        </div>
                    </div>
                }
                if let Some(line) = selected_line {
                    <LineInspector line={Rc::new(line.clone())} system={system_ref.clone()}
                        blocks={Rc::new(system.map(|s| s.blocks.clone()).unwrap_or_default())}
                        on_edit={on_edit.clone()}
                        recorded={results.as_ref().zip(run.clone()).zip(line.src.as_ref()).and_then(|((r, run), src)| {
                            let indices = run.signal_indices(&props.model, &path, src);
                            (!indices.is_empty()).then(|| Rc::new(Recorded {
                                run,
                                indices,
                                stale,
                                on_plot: r.on_plot.clone(),
                            }))
                        })} />
                }
                if let Some((index, annotation)) = selected_annotation {
                    // Deleting it frees its index for the next annotation, so
                    // the selection must not linger on that index.
                    <AnnotationInspector {index} annotation={Rc::new(annotation.clone())}
                        system={system_ref.clone()} on_edit={on_edit.clone().map(|on_edit| {
                            let selection = selection.clone();
                            Callback::from(move |edit: Edit| {
                                match (&edit, &*selection) {
                                    (Edit::DeleteAnnotation { .. }, _) => {
                                        selection.set(Selection::Nothing)
                                    }
                                    // Keep it selected with its new text.
                                    (
                                        Edit::SetAnnotationText { text, .. },
                                        Selection::Annotation(i, expected),
                                    ) => {
                                        let mut edited = expected.clone();
                                        edited.text = text.clone();
                                        selection.set(Selection::Annotation(*i, edited));
                                    }
                                    _ => {}
                                }
                                on_edit.emit(edit);
                            })
                        })} />
                }
                if let Some(b) = selected_block {
                    <Inspector block={Rc::new(b.clone())} chart={selected_chart.map(|c| Rc::new(c.clone()))}
                        system={system_ref.clone()} lines={Rc::new(system.map(|s| s.lines.clone()).unwrap_or_default())}
                        model={props.model.clone()} problems={Rc::new(block_problems)}
                        on_edit={on_edit.clone()} on_open={Callback::from({
                        let path = path.clone();
                        move |name: String| {
                            let mut p = (*path).clone();
                            p.push(name);
                            path.set(p);
                        }
                    })} />
                }
                // Closing an inspector clears what it shows, as Esc does.
                if selected_line.is_some() || selected_annotation.is_some() || selected_block.is_some() {
                    <button class="tool icon inspector-close" title="Close (Esc)" aria-label="Close the inspector"
                        onclick={{
                            let selection = selection.clone();
                            Callback::from(move |_: MouseEvent| selection.set(Selection::Nothing))
                        }}>{ icons::close() }</button>
                }
            </div>
            { for problem_list }
        </div>
    }
}

/// A recorded value as port value labels show it: four significant digits,
/// exponent form outside [1e-3, 1e5).
fn short_value(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e5).contains(&a) {
        return format!("{v:.3e}");
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0" } else { s }.to_string()
}

/// The values of recorded signals `indices` at the run's chosen sample, as
/// one label: a scalar, or the first few components of a vector.
fn value_label(run: &ShownRun, indices: &[usize]) -> Option<String> {
    let at = |i: usize| {
        run.signals
            .get(i)?
            .values
            .get(run.sample_index)
            .copied()
            .map(short_value)
    };
    match indices {
        [] => None,
        [i] => at(*i),
        _ => {
            let shown: Vec<String> = indices.iter().take(3).filter_map(|&i| at(i)).collect();
            let more = if indices.len() > 3 { ", …" } else { "" };
            Some(format!("[{}{more}]", shown.join(", ")))
        }
    }
}

/// Where the shown system's lines get port value labels, and the recorded
/// signals each shows: by its source port. This depends on the run's
/// signals, not on which sample is shown.
fn port_sources(
    model: &Model,
    path: &[String],
    sys: &System,
    run: &ShownRun,
) -> Vec<(Point, Vec<usize>)> {
    sys.lines
        .iter()
        .filter_map(|line| {
            let src = line.src.as_ref()?;
            let block = sys.block(&src.block)?;
            let indices = run.signal_indices(model, path, src);
            let p = geometry::port_anchor(block, src.port);
            (!indices.is_empty()).then(|| (Point::new(p.x + 3.0, p.y - 4.0), indices))
        })
        .collect()
}

#[derive(Properties, PartialEq)]
struct HoverCardProps {
    /// The model, and the block shown by the system it is in and its ID:
    /// looked up rather than copied, as a subsystem holds its contents.
    model: Rc<Model>,
    path: Vec<String>,
    id: BlockId,
    /// Position inside the diagram container, in pixels.
    x: f64,
    y: f64,
}

/// What a block is, shown while the pointer rests on it: its name, type,
/// ports and first few parameters.
#[function_component(HoverCard)]
fn hover_card(props: &HoverCardProps) -> Html {
    const SHOWN: usize = 4;
    let refs: Vec<&str> = props.path.iter().map(String::as_str).collect();
    let Some(b) = props
        .model
        .system_at(&refs)
        .and_then(|s| s.block(&props.id))
    else {
        return html! {};
    };
    let descriptor = catalog::find(&b.block_type).filter(|_| b.mask.is_none());
    let parameters: Vec<(String, String)> = match descriptor {
        Some(d) => d
            .parameters
            .iter()
            .filter_map(|p| Some((p.label.to_string(), b.param(p.name)?.to_string())))
            .take(SHOWN)
            .collect(),
        None => b
            .parameters
            .iter()
            .filter(|(k, _)| !HIDDEN_PARAMETERS.contains(&k.as_str()))
            .take(SHOWN)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };
    let (inputs, outputs) = (b.ports.inputs, b.ports.outputs);
    let plural = |n: u32, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    html! {
        <div class="hover-card" style={format!("left: {}px; top: {}px;", props.x, props.y)}>
            <div class="hover-name">{ b.name.replace('\n', " ") }</div>
            <div class="muted">
                { b.stateflow_type().unwrap_or_else(|| b.display_type()) }
                { format!(" · {} · {}", plural(inputs, "input"), plural(outputs, "output")) }
            </div>
            if !parameters.is_empty() {
                <table>
                    { for parameters.iter().map(|(k, v)| html! {
                        <tr><td class="muted">{ k }</td><td class="mono">{ v }</td></tr>
                    }) }
                </table>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct ShortcutHelpProps {
    editable: bool,
    on_close: Callback<()>,
}

/// The keyboard and mouse shortcuts, those for editing only when editing.
#[function_component(ShortcutHelp)]
fn shortcut_help(props: &ShortcutHelpProps) -> Html {
    const VIEW: &[(&str, &str)] = &[
        ("Scroll", "Zoom about the pointer"),
        ("Drag, or middle-drag", "Pan (middle-drag while editing)"),
        ("Space", "Fit to view"),
        ("Ctrl+= / Ctrl+-", "Zoom in / out"),
        ("Double-click a subsystem", "Open it"),
        ("Ctrl+F", "Find blocks anywhere in the model"),
        (
            "Tab / Shift+Tab",
            "With a block selected, select the next / previous",
        ),
        ("Esc", "Clear the selection, or go up a level"),
        ("?", "Show these shortcuts"),
    ];
    const EDIT: &[(&str, &str)] = &[
        ("Drag on empty canvas", "Select by box; Shift adds"),
        ("Shift+click", "Add to or remove from the selection"),
        ("Double-click empty canvas", "Find and add a block"),
        ("Double-click a block", "Edit its parameters"),
        ("Drag from a port", "Connect to another port"),
        ("Ctrl+drag a line", "Branch it"),
        ("Ctrl+click a block", "Connect the selected block to it"),
        ("Drag a line segment", "Reroute it"),
        ("Ctrl+drag a block", "Copy it there"),
        ("Ctrl+X / Ctrl+C / Ctrl+V", "Cut / copy / paste"),
        ("Ctrl+D", "Duplicate"),
        ("F2", "Rename the selected block"),
        ("Delete", "Delete the selection"),
        ("Arrows", "Nudge by a grid step (Shift: one unit)"),
        ("Ctrl+R / Ctrl+I", "Rotate / flip"),
        ("Ctrl+G / Ctrl+Shift+G", "Group into a subsystem / expand"),
        ("Ctrl+A", "Select all blocks"),
        ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
        ("Ctrl+S", "Save as a new version"),
    ];
    let rows = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(keys, what)| html! { <tr><td><kbd>{ *keys }</kbd></td><td>{ *what }</td></tr> })
            .collect::<Html>()
    };
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());
    html! {
        <div class="shortcut-help" onmousedown={stop.clone()} onclick={stop.clone()} ondblclick={stop}>
            <div class="shortcut-head">
                <strong>{ "Shortcuts" }</strong>
                <button class="tool icon" title="Close" onclick={props.on_close.reform(|_: MouseEvent| ())}>
                    { icons::close() }
                </button>
            </div>
            <table>
                { rows(VIEW) }
                if props.editable {
                    <tr><th colspan="2">{ "Editing" }</th></tr>
                    { rows(EDIT) }
                }
            </table>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct TreeProps {
    model: Rc<Model>,
    current: Vec<String>,
    on_open: Callback<Vec<String>>,
}

/// The model's subsystems as a tree, as Simulink's model browser shows it:
/// levels open along the way to the shown system and closed elsewhere,
/// until toggled.
#[function_component(SystemTree)]
fn system_tree(props: &TreeProps) -> Html {
    // Levels whose open state the user flipped from that default.
    let toggled = use_state(std::collections::HashSet::<Vec<String>>::new);
    let toggle = {
        let toggled = toggled.clone();
        Callback::from(move |p: Vec<String>| {
            let mut t = (*toggled).clone();
            if !t.remove(&p) {
                t.insert(p);
            }
            toggled.set(t);
        })
    };
    fn level(
        sys: &System,
        prefix: &[String],
        props: &TreeProps,
        toggled: &std::collections::HashSet<Vec<String>>,
        toggle: &Callback<Vec<String>>,
    ) -> Html {
        let subs: Vec<(&Block, &System)> = sys
            .blocks
            .iter()
            .filter_map(|b| Some((b, b.subsystem.as_deref()?)))
            .collect();
        if subs.is_empty() {
            return html! {};
        }
        html! {
            <ul>
                { for subs.into_iter().map(|(b, sub)| {
                    let mut p = prefix.to_vec();
                    p.push(b.name.clone());
                    let active = p == props.current;
                    let on_the_way = props.current.starts_with(&p) && !active;
                    let open = on_the_way != toggled.contains(&p);
                    let nested = sub.blocks.iter().any(|b| b.subsystem.is_some());
                    let on_open = props.on_open.clone();
                    let target = p.clone();
                    let flip = toggle.reform({
                        let p = p.clone();
                        move |_: MouseEvent| p.clone()
                    });
                    html! {
                        <li>
                            if nested {
                                <button class={classes!("twisty", open.then_some("open"))} onclick={flip}
                                    aria-expanded={open.to_string()}
                                    aria-label={if open { "Collapse" } else { "Expand" }} />
                            } else {
                                <span class="twisty" />
                            }
                            <a class={classes!(active.then_some("active"))}
                                onclick={Callback::from(move |_: MouseEvent| on_open.emit(target.clone()))}>
                                { b.name.replace('\n', " ") }
                            </a>
                            if open {
                                { level(sub, &p, props, toggled, toggle) }
                            }
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
            { level(&props.model.root, &[], props, &toggled, &toggle) }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct AnnotationInspectorProps {
    /// Index of the annotation in its system.
    index: usize,
    annotation: Rc<Annotation>,
    system: SystemRef,
    on_edit: Option<Callback<Edit>>,
}

/// The selected annotation: its text, editable unless it is rich text, and
/// a delete button.
#[function_component(AnnotationInspector)]
fn annotation_inspector(props: &AnnotationInspectorProps) -> Html {
    let a = &props.annotation;
    let target = AnnotationTarget {
        index: props.index,
        expected: (**a).clone(),
    };
    let interpreter = a.properties.get("Interpreter").filter(|i| !i.is_empty());
    let rich = a.rich_text || interpreter.is_some_and(|i| i == "rich");
    let label = match interpreter {
        Some(i) if !rich && i != "off" => format!("Source text (Interpreter: {i})"),
        _ => "Text".into(),
    };
    let text = match (&props.on_edit, rich) {
        (Some(on_edit), false) => {
            let (on_edit, system, target, current) = (
                on_edit.clone(),
                props.system.clone(),
                target.clone(),
                a.text.clone(),
            );
            let onchange = Callback::from(move |e: Event| {
                let text = e.target_unchecked_into::<HtmlTextAreaElement>().value();
                if text != current {
                    on_edit.emit(Edit::SetAnnotationText {
                        system: system.clone(),
                        target: target.clone(),
                        text,
                    });
                }
            });
            html! { <textarea class="annotation-text" value={a.text.clone()} rows="4" {onchange} /> }
        }
        (_, true) => html! {
            <p class="muted">{ "Rich text; it can be moved or deleted but not edited here." }</p>
        },
        (None, false) => html! { <pre>{ a.text.clone() }</pre> },
    };
    let delete = props.on_edit.clone().map(|on_edit| {
        let system = props.system.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            on_edit.emit(Edit::DeleteAnnotation {
                system: system.clone(),
                target: target.clone(),
            })
        });
        html! { <button class="danger" {onclick}>{ "Delete annotation" }</button> }
    });
    html! {
        <aside class="inspector">
            <h3>{ "Annotation" }</h3>
            <h4>{ label }</h4>
            { text }
            { for delete }
        </aside>
    }
}

#[derive(Properties, PartialEq)]
struct LineInspectorProps {
    line: Rc<Line>,
    /// The system containing the line.
    system: SystemRef,
    /// Blocks of that system, to name the line's ends.
    blocks: Rc<Vec<Block>>,
    on_edit: Option<Callback<Edit>>,
    /// What the shown run recorded for the line's signal.
    #[prop_or_default]
    recorded: Option<Rc<Recorded>>,
}

/// A line's signal as the shown run recorded it.
struct Recorded {
    run: Rc<ShownRun>,
    /// The run's signals carrying the line's value, one per component.
    indices: Vec<usize>,
    /// The diagram changed since the run.
    stale: bool,
    /// Plots a recorded signal by ID in the simulation pane.
    on_plot: Callback<String>,
}

/// By the run's identity rather than its samples, which are immutable and
/// may be many (and NaN, never equal to itself).
impl PartialEq for Recorded {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.run, &other.run)
            && self.indices == other.indices
            && self.stale == other.stale
            && self.on_plot == other.on_plot
    }
}

/// A sparkline of components `indices` of `run`, with the chosen sample
/// marked.
fn sparkline(run: &ShownRun, indices: &[usize]) -> Html {
    const W: f64 = 220.0;
    const H: f64 = 44.0;
    // Points drawn at most per component.
    const MAX_POINTS: usize = 300;
    let time = &run.time;
    let (Some(&t0), Some(&t1)) = (time.first(), time.last()) else {
        return html! {};
    };
    let series: Vec<&Vec<f64>> = indices
        .iter()
        .take(3)
        .filter_map(|&i| Some(&*run.signals.get(i)?.values))
        .collect();
    let finite = series
        .iter()
        .flat_map(|s| s.iter())
        .filter(|v| v.is_finite());
    let (lo, hi) = finite.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
        (lo.min(v), hi.max(v))
    });
    if !lo.is_finite() {
        return html! {};
    }
    // In units of the largest magnitude, so the span stays finite near
    // ±f64::MAX and nonzero for the tiniest values.
    let scale = match lo.abs().max(hi.abs()) {
        m if m > 0.0 => m,
        _ => 1.0,
    };
    let (low, high) = (lo / scale, hi / scale);
    let span = if high > low { high - low } else { 1.0 };
    let x = |t: f64| {
        if t1 > t0 {
            (t - t0) / (t1 - t0) * W
        } else {
            0.0
        }
    };
    let y = |v: f64| H - 2.0 - (v / scale - low) / span * (H - 4.0);
    let colors = ["#7aa2f7", "#9ece6a", "#e0af68"];
    let lines = series.iter().zip(colors).flat_map(|(s, color)| {
        sparkline_runs(time, s, MAX_POINTS)
            .into_iter()
            .map(move |run| {
                let points: Vec<String> = run
                    .iter()
                    .map(|&(t, v)| format!("{:.1},{:.1}", x(t), y(v)))
                    .collect();
                html! { <polyline points={points.join(" ")} stroke={color} fill="none" stroke-width="1.25" /> }
            })
            .collect::<Vec<_>>()
    });
    let marker = time.get(run.sample_index).map(|&t| {
        html! { <line class="sample-mark" x1={x(t).to_string()} x2={x(t).to_string()} y1="0" y2={H.to_string()} /> }
    });
    let label = format!(
        "Recorded from t = {} to {} s, between {} and {}",
        short_value(t0),
        short_value(t1),
        short_value(lo),
        short_value(hi)
    );
    html! {
        <svg class="sparkline" viewBox={format!("0 0 {W} {H}")} width={W.to_string()} height={H.to_string()}
            role="img" aria-label={label}>
            { for lines }
            { for marker }
        </svg>
    }
}

/// The samples of `values` over `time` a sparkline draws: at most about
/// `max` of them, evenly picked and always including the last, split into
/// runs at missing or non-finite values so gaps are not drawn across.
fn sparkline_runs(time: &[f64], values: &[f64], max: usize) -> Vec<Vec<(f64, f64)>> {
    let n = time.len().min(values.len());
    if n == 0 {
        return Vec::new();
    }
    let step = n.div_ceil(max.max(1)).max(1);
    let mut runs = vec![Vec::new()];
    // A gap anywhere since the last drawn sample, picked or not, breaks
    // the line there.
    let mut gap = false;
    for i in 0..n {
        let (t, v) = (time[i], values[i]);
        if !(t.is_finite() && v.is_finite()) {
            gap = true;
            continue;
        }
        if i % step != 0 && i != n - 1 {
            continue;
        }
        if gap && !runs.last().unwrap().is_empty() {
            runs.push(Vec::new());
        }
        gap = false;
        runs.last_mut().unwrap().push((t, v));
    }
    runs.retain(|r| !r.is_empty());
    runs
}

/// The selected line: where it runs, and its signal name (editable).
#[function_component(LineInspector)]
fn line_inspector(props: &LineInspectorProps) -> Html {
    let line = &props.line;
    let describe = |ep: &Endpoint| {
        let block = props
            .blocks
            .iter()
            .find(|b| b.id == ep.block)
            .map_or(ep.block.0.as_str(), |b| b.name.as_str());
        format!(
            "{} ({} {})",
            block.replace('\n', " "),
            ep.port.kind.token(),
            ep.port.index
        )
    };
    let mut ends = Vec::new();
    let mut pending = vec![(line.dst.as_ref(), &line.branches)];
    while let Some((dst, branches)) = pending.pop() {
        ends.extend(dst);
        pending.extend(branches.iter().map(|b| (b.dst.as_ref(), &b.branches)));
    }
    let name = line.name.clone().unwrap_or_default();
    let name_field = match (&props.on_edit, &line.src) {
        (Some(on_edit), Some(src)) => {
            let (on_edit, system, src, current) = (
                on_edit.clone(),
                props.system.clone(),
                src.clone(),
                name.clone(),
            );
            // An empty name removes the label.
            let onchange = Callback::from(move |e: Event| {
                let name = e.target_unchecked_into::<HtmlInputElement>().value();
                if name != current {
                    on_edit.emit(Edit::SetSignalName {
                        system: system.clone(),
                        src: src.clone(),
                        name,
                    });
                }
            });
            html! { <input class="param signal-name" value={name} placeholder="unnamed" {onchange} /> }
        }
        _ if name.is_empty() => html! { <span class="muted">{ "unnamed" }</span> },
        _ => html! { <code>{ name }</code> },
    };
    html! {
        <aside class="inspector">
            <h3>{ "Line" }</h3>
            <table>
                <tr><th>{ "Signal name" }</th><td>{ name_field }</td></tr>
                <tr><th>{ "From" }</th><td>{ line.src.as_ref().map(describe).unwrap_or_else(|| "(none)".into()) }</td></tr>
                { for ends.iter().map(|ep| html! { <tr><th>{ "To" }</th><td>{ describe(ep) }</td></tr> }) }
            </table>
            if let Some(r) = &props.recorded {
                <section class="recorded">
                    <h4>{ format!("Recorded in the v{} run", r.run.version) }</h4>
                    if r.stale {
                        <p class="muted">{ "The diagram has changed since this run." }</p>
                    }
                    { sparkline(&r.run, &r.indices) }
                    <table>
                        { for r.indices.iter().filter_map(|&i| {
                            let s = r.run.signals.get(i)?;
                            let t = r.run.time.get(r.run.sample_index).copied().unwrap_or_default();
                            let v = s.values.get(r.run.sample_index).copied();
                            let id = s.id.clone();
                            let on_plot = r.on_plot.reform(move |_: MouseEvent| id.clone());
                            Some(html! {
                                <tr>
                                    <td class="mono">{ v.map(short_value).unwrap_or_default() }</td>
                                    <td class="muted">{ format!("at t = {}", short_value(t)) }</td>
                                    <td><button class="tool" onclick={on_plot} title={format!("Plot {} in the simulation pane", s.name)}>{ "Plot" }</button></td>
                                </tr>
                            })
                        }) }
                    </table>
                </section>
            }
        </aside>
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
    /// The whole model, to tell whether deleting a subsystem's port block
    /// cuts a connection outside it.
    model: Rc<Model>,
    /// Problems found with the block.
    problems: Rc<Vec<Diagnostic>>,
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
    // Expanding is offered for any subsystem; the edit refuses ones it
    // cannot flatten faithfully and says why.
    let expand = props
        .on_edit
        .clone()
        .filter(|_| b.subsystem.is_some() && props.chart.is_none())
        .map(|on_edit| {
            let (system, id) = (props.system.clone(), b.id.clone());
            let onclick = Callback::from(move |_: MouseEvent| {
                on_edit.emit(Edit::ExpandSubsystem {
                    system: system.clone(),
                    id: id.clone(),
                })
            });
            html! { <button {onclick} title="Ctrl+Shift+G">{ "Expand subsystem" }</button> }
        });
    let kind = b.stateflow_type().unwrap_or_else(|| b.display_type());
    let script = props.chart.as_ref().and_then(|c| c.script.clone());
    // Native catalog blocks get their dialog; any other stored parameters,
    // and every parameter of other blocks, are listed raw.
    let descriptor = catalog::find(&b.block_type)
        .filter(|_| b.mask.is_none() && b.library_source.is_none() && b.subsystem.is_none());
    let others: Vec<(&String, &String)> = b
        .parameters
        .iter()
        .filter(|(k, _)| !HIDDEN_PARAMETERS.contains(&k.as_str()))
        .filter(|(k, _)| {
            descriptor.is_none_or(|d| d.parameters.iter().all(|p| p.name != k.as_str()))
        })
        .collect();
    let shown_fields: Vec<&str> = descriptor
        .map(|d| {
            d.dialog_sections(&b.parameters)
                .into_iter()
                .flat_map(|s| s.fields)
                .filter(|(_, visible)| *visible)
                .map(|(p, _)| p.name)
                .collect()
        })
        .unwrap_or_default();
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
        let (system, id, lines, model) = (
            props.system.clone(),
            b.id.clone(),
            props.lines.clone(),
            props.model.clone(),
        );
        let onclick = Callback::from(move |_: MouseEvent| {
            for edit in confirm_delete(&model, &system, std::slice::from_ref(&id), &lines)
                .unwrap_or_default()
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
            // Problems with a shown dialog field are shown beside it instead.
            { for props.problems.iter().filter(|d| !matches!(&d.target,
                DiagnosticTarget::Block { parameter: Some(p), .. } if shown_fields.contains(&p.as_str())
            )).map(|d| {
                let about = match &d.target {
                    DiagnosticTarget::Block { parameter: Some(p), .. } => format!("{p}: "),
                    _ => String::new(),
                };
                html! { <div class={problem_class(d)}>{ about }{ &d.message }</div> }
            }) }
            { for open }
            { for expand }
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
            if descriptor.is_some() {
                <ParameterDialog block={props.block.clone()} system={props.system.clone()}
                    problems={props.problems.clone()} on_edit={props.on_edit.clone()} />
            }
            if !others.is_empty() {
                <details class="raw" open={descriptor.is_none()}>
                    <summary>{ if descriptor.is_some() { "Other parameters" } else { "Parameters" } }</summary>
                    <table>
                        { for others.iter().map(|(k, v)| html! {
                            <tr><td>{ k }</td><td>{ value_cell(k, v) }</td></tr>
                        }) }
                    </table>
                </details>
            }
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

pub(crate) fn fmt_num(v: f64) -> String {
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
            type_defaults: Default::default(),
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

    #[test]
    fn sparklines_keep_the_last_sample_and_break_at_gaps() {
        let time: Vec<f64> = (0..1001).map(f64::from).collect();
        let values: Vec<f64> = time.clone();
        let runs = sparkline_runs(&time, &values, 300);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].len() <= 301, "{}", runs[0].len());
        assert_eq!(runs[0].last(), Some(&(1000.0, 1000.0)));
        let gappy = [1.0, f64::NAN, 3.0, 4.0];
        let runs = sparkline_runs(&[0.0, 1.0, 2.0, 3.0], &gappy, 300);
        assert_eq!(runs, vec![vec![(0.0, 1.0)], vec![(2.0, 3.0), (3.0, 4.0)]]);
        // A gap between drawn samples breaks the line too.
        let time: Vec<f64> = (0..600).map(f64::from).collect();
        let mut ones = vec![1.0; 600];
        ones[1] = f64::NAN;
        let runs = sparkline_runs(&time, &ones, 300);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], vec![(0.0, 1.0)]);
        assert!(sparkline_runs(&[], &[], 300).is_empty());
    }

    #[test]
    fn blocks_line_up_and_spread() {
        let b = |id: &str, l: f64, t: f64, w: f64| {
            (BlockId::from(id), Rect::new(l, t, l + w, t + 20.0))
        };
        let three = [
            b("1", 0.0, 0.0, 30.0),
            b("2", 50.0, 13.0, 30.0),
            b("3", 200.0, 40.0, 10.0),
        ];
        let moved = |how| -> Vec<(String, Rect)> {
            arranged(&three, how)
                .into_iter()
                .map(|(id, r)| (id.0, r))
                .collect()
        };
        // Tops on the topmost; the first stays, so it is left out.
        let top = moved(Arrange::Top);
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|(_, r)| r.top == 0.0));
        // Centres on the first selected's centre (15).
        assert!(moved(Arrange::Center)
            .iter()
            .all(|(_, r)| (r.left + r.right) / 2.0 == 15.0));
        // Right edges on the rightmost (210).
        assert!(moved(Arrange::Right).iter().all(|(_, r)| r.right == 210.0));
        // Equal gaps between 0..210 with 70 of blocks: 70 each.
        let across = moved(Arrange::Across);
        assert_eq!(
            across,
            vec![("2".to_string(), Rect::new(100.0, 13.0, 130.0, 33.0))]
        );
        assert!(arranged(&three[..1], Arrange::Left).is_empty());
    }

    #[test]
    fn values_are_labelled_briefly() {
        assert_eq!(short_value(2.0), "2");
        assert_eq!(short_value(-0.5), "-0.5");
        assert_eq!(short_value(1.0 / 3.0), "0.3333");
        assert_eq!(short_value(-0.00001), "-1.000e-5");
        assert_eq!(short_value(123456.0), "1.235e5");
        assert_eq!(short_value(-0.0), "0");
        assert_eq!(short_value(f64::NAN), "NaN");
    }

    #[test]
    fn lines_are_labelled_with_recorded_values() {
        use crate::results::ShownSignal;
        let mdl = "Model {\n Name m\n System {\n Name m\n Block {\n BlockType Constant\n Name c\n SID 1\n Ports [0,1]\n Value [1 2]\n Position [0, 0, 30, 30]\n }\n Block {\n BlockType Demux\n Name d\n SID 2\n Ports [1,2]\n Outputs 2\n Position [100, 0, 105, 30]\n }\n Line {\n SrcBlock c\n SrcPort 1\n DstBlock d\n DstPort 1\n }\n }\n}";
        let model = unlinked_import::import_mdl("m.mdl", mdl).unwrap();
        let signal = |id: &str, v: f64| ShownSignal {
            id: id.into(),
            name: id.into(),
            values: Rc::new(vec![0.0, v]),
        };
        let run = ShownRun {
            run_id: uuid::Uuid::nil(),
            version: 1,
            time: Rc::new(vec![0.0, 1.0]),
            signals: Rc::new(vec![signal("1[1]", 1.0), signal("1[2]", 2.5)]),
            sample_index: 1,
        };
        let sources = port_sources(&model, &[], &model.root, &run);
        assert_eq!(sources.len(), 1);
        assert_eq!(
            value_label(&run, &sources[0].1).as_deref(),
            Some("[1, 2.5]")
        );
    }

    #[test]
    fn guides_line_up_ports_within_reach() {
        let mdl = "Model {\n Name m\n System {\n Name m\n Block {\n BlockType Gain\n Name a\n SID 1\n Ports [1, 1]\n Position [0, 0, 30, 30]\n }\n Block {\n BlockType Gain\n Name b\n SID 2\n Ports [1, 1]\n Position [100, 40, 130, 70]\n }\n }\n}\n";
        let model = unlinked_import::import_mdl("m.mdl", mdl).unwrap();
        let moving = [BlockId::from("2")];
        // Dragged up to 3 units below a's output: lined up exactly.
        let ((dx, dy), guides) = align(&model.root, &moving, (0.0, -37.0), 6.0);
        assert_eq!((dx, dy), (0.0, -40.0));
        assert!(
            matches!(guides[..], [Guide::Horizontal { y, .. }] if y == 15.0),
            "{guides:?}"
        );
        // Out of reach: left alone.
        let (offset, guides) = align(&model.root, &moving, (0.0, -18.0), 0.5);
        assert_eq!(offset, (0.0, -18.0));
        assert!(guides.is_empty());
    }

    #[test]
    fn connections_snap_within_reach_to_free_ports() {
        let mut mdl = String::from("Model {\n Name m\n System {\n Name m\n");
        for (id, x) in [("1", 0), ("2", 100), ("3", 200)] {
            mdl += &format!(
                " Block {{\n BlockType Gain\n Name b{id}\n SID {id}\n Ports [1, 1]\n Position [{x}, 0, {}, 30]\n }}\n",
                x + 30
            );
        }
        mdl += " }\n}\n";
        let mut model = unlinked_import::import_mdl("m.mdl", &mdl).unwrap();
        let ep = |id: &str, kind| Endpoint {
            block: id.into(),
            port: PortRef { kind, index: 1 },
        };
        let from = ep("1", PortKind::Out);
        let to = ep("2", PortKind::In);
        let anchor = geometry::port_anchor(&model.root.blocks[1], to.port);
        let near = Point::new(anchor.x - 20.0, anchor.y);
        // Within reach, and not beyond it.
        assert_eq!(
            snap_port(&model.root, &from, near, 24.0, None).unwrap().0,
            to
        );
        assert!(snap_port(&model.root, &from, near, 10.0, None).is_none());
        // Over a block, its nearest free port at any distance.
        let over = Some(&BlockId::from("2"));
        let body = Point::new(125.0, 15.0);
        assert_eq!(
            snap_port(&model.root, &from, body, 1.0, over).unwrap().0,
            to
        );
        // A driven input is no target.
        model.root.lines.push(unlinked_model::Line {
            src: Some(from.clone()),
            dst: Some(to),
            ..Default::default()
        });
        assert!(snap_port(&model.root, &from, anchor, 24.0, over).is_none());
        // Dragged from an input, the end snaps to outputs.
        let output = geometry::port_anchor(
            &model.root.blocks[1],
            PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        );
        let reverse = ep("3", PortKind::In);
        assert_eq!(
            snap_port(&model.root, &reverse, output, 24.0, None)
                .unwrap()
                .0,
            ep("2", PortKind::Out)
        );
    }
}
