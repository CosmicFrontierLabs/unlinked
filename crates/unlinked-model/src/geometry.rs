//! Port placement on block outlines.
//!
//! Simulink spreads the ports of one side evenly along that side: with `n`
//! ports on a side of length `L`, port `i` (1-based) sits at `(i - 0.5) * L / n`
//! from the side's start, snapped to the 5 pixel grid (confirmed against the
//! stored line routing of the test corpus). Round Sum blocks are the
//! exception; see [`port_on_outline`]. Ports are always numbered left-to-right on
//! horizontal sides and top-to-bottom on vertical sides. Line vertices in
//! model files are stored relative to the port anchor, which is
//! [`PORT_OFFSET`] outside the block outline.

use crate::{Block, Orientation, Point, PortKind, PortRef, Rect};

/// Distance from the block outline to a port's line anchor.
pub const PORT_OFFSET: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// Unit vector pointing out of the block through this side.
    pub fn outward(&self) -> (f64, f64) {
        match self {
            Side::Left => (-1.0, 0.0),
            Side::Right => (1.0, 0.0),
            Side::Top => (0.0, -1.0),
            Side::Bottom => (0.0, 1.0),
        }
    }

    fn opposite(&self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// Orientation and control-side mirroring for Simulink's `BlockRotation`
/// (clockwise degrees) and `BlockMirror`.
pub fn from_rotation(rotation: i32, mirror: bool) -> (Orientation, bool) {
    match (rotation.rem_euclid(360), mirror) {
        (90, false) => (Orientation::Down, false),
        (90, true) => (Orientation::Up, true),
        (180, false) => (Orientation::Left, true),
        (180, true) => (Orientation::Right, true),
        (270, false) => (Orientation::Up, false),
        (270, true) => (Orientation::Down, true),
        (_, false) => (Orientation::Right, false),
        (_, true) => (Orientation::Left, false),
    }
}

/// Inverse of [`from_rotation`]: `BlockRotation` and `BlockMirror`.
pub fn to_rotation(orientation: Orientation, mirrored: bool) -> (i32, bool) {
    match (orientation, mirrored) {
        (Orientation::Right, false) => (0, false),
        (Orientation::Left, false) => (0, true),
        (Orientation::Down, false) => (90, false),
        (Orientation::Up, true) => (90, true),
        (Orientation::Left, true) => (180, false),
        (Orientation::Right, true) => (180, true),
        (Orientation::Up, false) => (270, false),
        (Orientation::Down, true) => (270, true),
    }
}

/// The block turned 90° clockwise, as Simulink's Rotate (Ctrl+R).
pub fn rotated(orientation: Orientation, mirrored: bool) -> (Orientation, bool) {
    let (rotation, mirror) = to_rotation(orientation, mirrored);
    from_rotation(rotation + 90, mirror)
}

/// The block flipped across its signal axis, as Simulink's Flip (Ctrl+I).
pub fn flipped(orientation: Orientation, mirrored: bool) -> (Orientation, bool) {
    let (rotation, mirror) = to_rotation(orientation, mirrored);
    from_rotation(rotation, !mirror)
}

/// The stored vertices of a drawn wire after dragging segment `segment`
/// (between `points[segment]` and the next) across itself by `delta`.
///
/// `points` is the wire as drawn: `fixed` leading points belong to its
/// source or junction, and `fixed_end` trailing ones to where it ends (a
/// destination port's anchor and outline point, or a trunk's junction).
/// Those stay put; an end of the segment that is fixed gains a connecting jog
/// instead. Repeated and collinear vertices are dropped. `None` when the
/// segment lies entirely within the fixed ends.
pub fn drag_segment(
    points: &[Point],
    fixed: usize,
    fixed_end: usize,
    segment: usize,
    delta: f64,
) -> Option<Vec<Point>> {
    let n = points.len();
    if fixed == 0 || fixed_end == 0 || n < fixed + fixed_end || segment + 1 >= n {
        return None;
    }
    if segment + 1 < fixed || segment + fixed_end > n - 1 {
        return None;
    }
    let (a, b) = (points[segment], points[segment + 1]);
    let horizontal = (b.x - a.x).abs() >= (b.y - a.y).abs();
    let shift = |p: Point| {
        if horizontal {
            Point::new(p.x, p.y + delta)
        } else {
            Point::new(p.x + delta, p.y)
        }
    };
    let mut out = points[..segment].to_vec();
    if segment < fixed {
        out.push(a);
    }
    out.push(shift(a));
    out.push(shift(b));
    if segment + 1 >= n - fixed_end {
        out.push(b);
    }
    out.extend_from_slice(&points[segment + 2..]);
    // Drop repeated and collinear interior points in one pass: each point is
    // judged against the last one kept and the one after it.
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    let mut kept: Vec<Point> = out[..fixed].to_vec();
    for i in fixed..out.len() - fixed_end {
        let (prev, here, next) = (kept[kept.len() - 1], out[i], out[i + 1]);
        let repeated = close(prev.x, here.x) && close(prev.y, here.y);
        let collinear = (close(prev.x, here.x) && close(here.x, next.x))
            || (close(prev.y, here.y) && close(here.y, next.y));
        if !repeated && !collinear {
            kept.push(here);
        }
    }
    Some(kept.split_off(fixed))
}

/// A block outline after a quarter turn: width and height swap about the
/// exact centre, so repeated turns never drift.
pub fn quarter_turn(r: Rect) -> Rect {
    let c = r.center();
    let (hw, hh) = (r.height() / 2.0, r.width() / 2.0);
    Rect::new(c.x - hw, c.y - hh, c.x + hw, c.y + hh)
}

/// Ports drawn on the "control" side (top, for a right-facing block).
const CONTROL_KINDS: [PortKind; 5] = [
    PortKind::Enable,
    PortKind::Trigger,
    PortKind::IfAction,
    PortKind::Reset,
    PortKind::State,
];

/// Which side of the block a port kind is drawn on.
pub fn port_side(orientation: Orientation, mirrored: bool, kind: PortKind) -> Side {
    let (input, control) = match orientation {
        Orientation::Right => (Side::Left, Side::Top),
        Orientation::Left => (Side::Right, Side::Top),
        Orientation::Down => (Side::Top, Side::Right),
        Orientation::Up => (Side::Bottom, Side::Left),
    };
    let control = if mirrored {
        control.opposite()
    } else {
        control
    };
    match kind {
        PortKind::In | PortKind::LConn => input,
        PortKind::Out | PortKind::RConn => input.opposite(),
        _ => control,
    }
}

/// Port kinds in the order they are laid out along a shared side.
const SIDE_ORDER: [PortKind; 9] = [
    PortKind::In,
    PortKind::LConn,
    PortKind::Out,
    PortKind::RConn,
    CONTROL_KINDS[0],
    CONTROL_KINDS[1],
    CONTROL_KINDS[2],
    CONTROL_KINDS[3],
    CONTROL_KINDS[4],
];

/// Zero-based slot of `port` along its side, and the number of slots there.
fn side_slot(block: &Block, side: Side, port: PortRef) -> (u64, u64) {
    let mut before = 0u64;
    let mut total = 0u64;
    for kind in SIDE_ORDER {
        if port_side(block.orientation, block.mirrored, kind) != side {
            continue;
        }
        let count = u64::from(block.ports.count(kind));
        if kind == port.kind {
            before = total + u64::from(port.index.clamp(1, block.ports.count(kind).max(1))) - 1;
        }
        total += count;
    }
    (before, total.max(1))
}

/// Simulink snaps port positions to a 5 pixel grid.
fn snap(v: f64) -> f64 {
    (v / 5.0 + 0.5).floor() * 5.0
}

/// Angle (degrees, counter-clockwise from +x with y up) of an input on a
/// round Sum block. The `Inputs` string's characters, including `|`
/// spacers, are spread evenly from the top through the left to the bottom
/// of the circle; e.g. `|++` puts inputs at the left and bottom.
fn round_sum_angle(block: &Block, port: PortRef) -> Option<f64> {
    if block.block_type != "Sum"
        || port.kind != PortKind::In
        || block.param("IconShape") != Some("round")
    {
        return None;
    }
    let inputs = block.param("Inputs").unwrap_or("|++").trim();
    let chars: Vec<char> = if inputs.chars().all(|c| c.is_ascii_digit()) {
        let n: usize = inputs.parse().ok()?;
        vec!['+'; n.min(1024)]
    } else {
        inputs
            .chars()
            .filter(|c| matches!(c, '+' | '-' | '|'))
            .collect()
    };
    let pos = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c != '|')
        .nth(port.index.checked_sub(1)? as usize)?
        .0;
    let mut angle = if chars.len() > 1 {
        90.0 + 180.0 * pos as f64 / (chars.len() - 1) as f64
    } else {
        180.0
    };
    if block.mirrored {
        angle = -angle;
    }
    Some(match block.orientation {
        Orientation::Right => angle,
        Orientation::Left => 180.0 - angle,
        Orientation::Down => angle - 90.0,
        Orientation::Up => angle + 90.0,
    })
}

/// Point on the block outline where the port sits.
pub fn port_on_outline(block: &Block, port: PortRef) -> Point {
    let r = &block.position;
    if let Some(angle) = round_sum_angle(block, port) {
        let c = r.center();
        let (s, co) = angle.to_radians().sin_cos();
        return Point::new(c.x + co * r.width() / 2.0, c.y - s * r.height() / 2.0);
    }
    let side = port_side(block.orientation, block.mirrored, port.kind);
    let (slot, n) = side_slot(block, side, port);
    let frac = (slot as f64 + 0.5) / n as f64;
    match side {
        Side::Left => Point::new(r.left, snap(r.top + frac * r.height())),
        Side::Right => Point::new(r.right, snap(r.top + frac * r.height())),
        Side::Top => Point::new(snap(r.left + frac * r.width()), r.top),
        Side::Bottom => Point::new(snap(r.left + frac * r.width()), r.bottom),
    }
}

/// Unit vector pointing away from the block at the port, the direction a
/// line leaves an output or enters an input (reversed).
pub fn port_outward(block: &Block, port: PortRef) -> (f64, f64) {
    if let Some(angle) = round_sum_angle(block, port) {
        let (s, c) = angle.to_radians().sin_cos();
        return (c, -s);
    }
    port_side(block.orientation, block.mirrored, port.kind).outward()
}

/// Line anchor for a port: [`PORT_OFFSET`] outside the outline.
pub fn port_anchor(block: &Block, port: PortRef) -> Point {
    let p = port_on_outline(block, port);
    let (dx, dy) = port_outward(block, port);
    Point::new(p.x + dx * PORT_OFFSET, p.y + dy * PORT_OFFSET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStyle, PortCounts, Rect};
    use std::collections::BTreeMap;

    #[test]
    fn rotation_mapping_roundtrips_and_composes() {
        let all = [
            Orientation::Right,
            Orientation::Left,
            Orientation::Up,
            Orientation::Down,
        ];
        for o in all {
            for m in [false, true] {
                let (r, mirror) = to_rotation(o, m);
                assert_eq!(from_rotation(r, mirror), (o, m));
                // Four turns or two flips are the identity.
                let mut s = (o, m);
                for _ in 0..4 {
                    s = rotated(s.0, s.1);
                }
                assert_eq!(s, (o, m));
                let f = flipped(o, m);
                assert_eq!(flipped(f.0, f.1), (o, m));
            }
        }
        assert_eq!(
            rotated(Orientation::Right, false),
            (Orientation::Down, false)
        );
        assert_eq!(
            flipped(Orientation::Right, false),
            (Orientation::Left, false)
        );
    }

    #[test]
    fn dragging_a_segment_moves_it_and_keeps_the_ends_attached() {
        let p = Point::new;
        // Source outline/anchor, an elbow pair, destination anchor/outline.
        let wire = [
            p(40.0, 20.0),
            p(45.0, 20.0),
            p(70.0, 20.0),
            p(70.0, 60.0),
            p(95.0, 60.0),
            p(100.0, 60.0),
        ];
        // The vertical middle segment moves right by 10.
        assert_eq!(
            drag_segment(&wire, 2, 2, 2, 10.0),
            Some(vec![p(80.0, 20.0), p(80.0, 60.0)])
        );
        // The first horizontal segment leaves the source anchor: the anchor
        // stays and a jog joins it to the moved segment.
        assert_eq!(
            drag_segment(&wire, 2, 2, 1, -10.0),
            Some(vec![p(45.0, 10.0), p(70.0, 10.0), p(70.0, 60.0)])
        );
        // The last segment into the port ends above the anchor; the jog down
        // to the anchor is drawn by the router.
        assert_eq!(
            drag_segment(&wire, 2, 2, 3, 5.0),
            Some(vec![p(70.0, 20.0), p(70.0, 65.0), p(95.0, 65.0)])
        );
        // Port stubs cannot be dragged.
        assert_eq!(drag_segment(&wire, 2, 2, 0, 5.0), None);
        assert_eq!(drag_segment(&wire, 2, 2, 4, 5.0), None);
        // A trunk ends at its junction, the one fixed end point: dragging
        // its last segment keeps the junction and adds a jog to it.
        let trunk = [p(40.0, 20.0), p(45.0, 20.0), p(70.0, 20.0), p(70.0, 60.0)];
        assert_eq!(
            drag_segment(&trunk, 2, 1, 2, 10.0),
            Some(vec![p(80.0, 20.0), p(80.0, 60.0)])
        );
        // Dragging onto the source's line merges the collinear vertices,
        // leaving one corner above the port.
        let straight = [
            p(40.0, 20.0),
            p(45.0, 20.0),
            p(70.0, 20.0),
            p(70.0, 30.0),
            p(95.0, 30.0),
            p(100.0, 30.0),
        ];
        assert_eq!(
            drag_segment(&straight, 2, 2, 3, -10.0),
            Some(vec![p(95.0, 20.0)])
        );
    }

    /// Dragging runs on every pointer move, so it must stay linear in the
    /// size of imported routes (from review: a quadratic pass took 1.7 s at
    /// 100k points).
    #[test]
    fn dragging_long_routes_stays_linear() {
        let n = 200_000;
        let mut wire = vec![Point::new(-5.0, 0.0), Point::new(0.0, 0.0)];
        wire.extend((1..n).map(|i| {
            let step = (i / 2) as f64 * 10.0;
            if i % 2 == 0 {
                Point::new(step, step)
            } else {
                Point::new(step + 10.0, step)
            }
        }));
        let last = *wire.last().unwrap();
        wire.push(Point::new(last.x, last.y + 5.0));
        wire.push(Point::new(last.x, last.y + 10.0));
        let start = std::time::Instant::now();
        let route = drag_segment(&wire, 2, 2, n / 2, 5.0).unwrap();
        assert!(route.len() > n / 2);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn quarter_turns_keep_the_centre() {
        let r = Rect::new(0.0, 0.0, 30.0, 35.0);
        let once = quarter_turn(r);
        assert_eq!(once, Rect::new(-2.5, 2.5, 32.5, 32.5));
        assert_eq!(once.center(), r.center());
        let four = (0..4).fold(r, |r, _| quarter_turn(r));
        assert_eq!(four, r);
    }

    fn block(pos: Rect, ports: &[u32], orientation: Orientation, mirrored: bool) -> Block {
        Block {
            id: "1".into(),
            block_type: "SubSystem".into(),
            name: "b".into(),
            position: pos,
            orientation,
            mirrored,
            ports: PortCounts::from_slice(ports),
            parameters: BTreeMap::new(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: BlockStyle::default(),
            interface: None,
        }
    }

    fn port(kind: PortKind, index: u32) -> PortRef {
        PortRef { kind, index }
    }

    #[test]
    fn right_facing_ports() {
        let b = block(
            Rect::new(0.0, 0.0, 40.0, 60.0),
            &[3, 1, 1],
            Orientation::Right,
            false,
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::In, 1)),
            Point::new(0.0, 10.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::In, 3)),
            Point::new(0.0, 50.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Out, 1)),
            Point::new(40.0, 30.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Enable, 1)),
            Point::new(20.0, 0.0)
        );
        assert_eq!(
            port_anchor(&b, port(PortKind::Out, 1)),
            Point::new(45.0, 30.0)
        );
        assert_eq!(
            port_anchor(&b, port(PortKind::Enable, 1)),
            Point::new(20.0, -5.0)
        );
    }

    #[test]
    fn left_facing_swaps_sides() {
        let b = block(
            Rect::new(0.0, 0.0, 40.0, 60.0),
            &[2, 1],
            Orientation::Left,
            false,
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::In, 1)),
            Point::new(40.0, 15.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Out, 1)),
            Point::new(0.0, 30.0)
        );
    }

    #[test]
    fn down_facing_numbers_left_to_right() {
        let b = block(
            Rect::new(0.0, 0.0, 60.0, 40.0),
            &[2, 1],
            Orientation::Down,
            false,
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::In, 1)),
            Point::new(15.0, 0.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::In, 2)),
            Point::new(45.0, 0.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Out, 1)),
            Point::new(30.0, 40.0)
        );
    }

    #[test]
    fn enable_and_trigger_share_top() {
        let b = block(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            &[1, 1, 1, 1],
            Orientation::Right,
            false,
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Enable, 1)),
            Point::new(10.0, 0.0)
        );
        assert_eq!(
            port_on_outline(&b, port(PortKind::Trigger, 1)),
            Point::new(30.0, 0.0)
        );
        let m = block(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            &[1, 1, 1],
            Orientation::Right,
            true,
        );
        assert_eq!(
            port_on_outline(&m, port(PortKind::Enable, 1)),
            Point::new(20.0, 40.0)
        );
    }
}
