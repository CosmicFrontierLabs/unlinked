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

use crate::{Block, Orientation, Point, PortKind, PortRef};

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
