//! Turn stored lines into drawable polylines.
//!
//! Stored vertices are drawn as-is. Newer Simulink releases only store
//! routing hints and route the final leg into a port automatically, so the
//! last leg gets an orthogonal elbow whenever the stored route does not
//! already line up with the port.

use unlinked_model::geometry::{port_anchor, port_on_outline, port_outward};
use unlinked_model::{Branch, Endpoint, Line, Point, PortKind, System};

/// Distance a synthesized route leaves a port before turning.
const STUB: f64 = 10.0;

pub struct Wire {
    pub points: Vec<Point>,
    /// Arrowhead tip and unit direction of travel at the tip.
    pub arrow: Option<(Point, (f64, f64))>,
    /// Missing source or destination (drawn as a dashed, red line).
    pub dangling: bool,
    /// The port this wire ends at, if it is a leaf of the line.
    pub dst: Option<Endpoint>,
}

pub struct RoutedLine {
    pub wires: Vec<Wire>,
    pub junctions: Vec<Point>,
    pub label: Option<(Point, String)>,
}

pub fn route_line(sys: &System, line: &Line) -> RoutedLine {
    let src_anchor = line.src.as_ref().and_then(|ep| {
        let b = sys.block(&ep.block)?;
        Some((
            port_anchor(b, ep.port),
            port_outward(b, ep.port),
            port_on_outline(b, ep.port),
        ))
    });

    let mut trunk: Vec<Point> = Vec::new();
    if let Some((anchor, _, outline)) = src_anchor {
        trunk.push(outline);
        trunk.push(anchor);
    }
    trunk.extend(&line.points);

    let mut out = RoutedLine {
        wires: Vec::new(),
        junctions: Vec::new(),
        label: None,
    };

    // Physical (Simscape) connection trees have no source; they are complete
    // when at least two conserving ports are joined.
    let mut ends = Vec::new();
    collect_ends(&line.dst, &line.branches, &mut ends);
    let physical = ends.iter().any(|e| is_physical(e));
    let endpoint_count = usize::from(src_anchor.is_some())
        + ends
            .iter()
            .filter(|ep| sys.block(&ep.block).is_some())
            .count();
    let incomplete = endpoint_count < 2 || (src_anchor.is_none() && !physical);

    if trunk.is_empty() {
        // No source and no trunk vertices: each branch starts at its own
        // (absolute) first vertex.
        for b in line.branches.iter().filter(|b| !b.points.is_empty()) {
            emit(
                sys,
                b.points.clone(),
                &b.dst,
                &b.branches,
                incomplete,
                &mut out,
            );
        }
        return out;
    }

    // With no stored vertices and a pure point-to-point line, synthesize a
    // Manhattan route leaving the source port along its outward direction.
    if line.points.is_empty() && line.branches.is_empty() {
        if let (Some((anchor, dir, _)), Some(dst)) = (src_anchor, &line.dst) {
            if let Some((dst_anchor, dst_dir, _)) = endpoint_geometry(sys, dst) {
                manhattan(&mut trunk, anchor, dir, dst_anchor, dst_dir);
            }
        }
    }

    if let Some(name) = line.name.as_deref().filter(|n| !n.is_empty()) {
        if trunk.len() >= 2 {
            let (a, b) = longest_leading_segment(&trunk);
            out.label = Some((
                Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0),
                name.to_string(),
            ));
        }
    }

    emit(sys, trunk, &line.dst, &line.branches, incomplete, &mut out);
    out
}

fn is_physical(ep: &Endpoint) -> bool {
    matches!(ep.port.kind, PortKind::LConn | PortKind::RConn)
}

fn collect_ends<'a>(
    dst: &'a Option<Endpoint>,
    branches: &'a [Branch],
    out: &mut Vec<&'a Endpoint>,
) {
    out.extend(dst.as_ref());
    for b in branches {
        collect_ends(&b.dst, &b.branches, out);
    }
}

fn endpoint_geometry(sys: &System, ep: &Endpoint) -> Option<(Point, (f64, f64), Point)> {
    let b = sys.block(&ep.block)?;
    Some((
        port_anchor(b, ep.port),
        port_outward(b, ep.port),
        port_on_outline(b, ep.port),
    ))
}

fn emit(
    sys: &System,
    mut path: Vec<Point>,
    dst: &Option<Endpoint>,
    branches: &[Branch],
    incomplete: bool,
    out: &mut RoutedLine,
) {
    let tail = *path.last().unwrap();
    let fan_out = branches.len() + usize::from(dst.is_some());
    if fan_out >= 2 {
        out.junctions.push(tail);
    }

    for b in branches {
        let mut sub = vec![tail];
        sub.extend(&b.points);
        emit(sys, sub, &b.dst, &b.branches, incomplete, out);
    }

    let geometry = dst
        .as_ref()
        .and_then(|d| endpoint_geometry(sys, d).map(|g| (d, g)));
    let arrow = match geometry {
        Some((d, (anchor, dir, outline))) => {
            finish_leg(&mut path, anchor, dir);
            path.push(outline);
            (!is_physical(d)).then_some((outline, (-dir.0, -dir.1)))
        }
        None => None,
    };
    if path.len() >= 2 {
        out.wires.push(Wire {
            points: path,
            dangling: incomplete || (geometry.is_none() && branches.is_empty()),
            arrow,
            dst: dst.clone(),
        });
    }
}

/// Add an elbow so the path arrives at `anchor` travelling against `dir`.
fn finish_leg(path: &mut Vec<Point>, anchor: Point, dir: (f64, f64)) {
    let last = *path.last().unwrap();
    let horizontal = dir.0.abs() > 0.9;
    let vertical = dir.1.abs() > 0.9;
    let aligned = |a: f64, b: f64| (a - b).abs() < 0.5;
    if horizontal && !aligned(last.y, anchor.y) {
        let outside = (last.x - anchor.x) * dir.0 >= 0.0;
        if outside {
            path.push(Point::new(last.x, anchor.y));
        } else {
            let x = anchor.x + dir.0 * STUB;
            path.push(Point::new(x, last.y));
            path.push(Point::new(x, anchor.y));
        }
    } else if vertical && !aligned(last.x, anchor.x) {
        let outside = (last.y - anchor.y) * dir.1 >= 0.0;
        if outside {
            path.push(Point::new(anchor.x, last.y));
        } else {
            let y = anchor.y + dir.1 * STUB;
            path.push(Point::new(last.x, y));
            path.push(Point::new(anchor.x, y));
        }
    }
    if !(aligned(last.x, anchor.x) && aligned(last.y, anchor.y)) {
        path.push(anchor);
    }
}

/// Route from a source anchor to a destination anchor with axis-aligned
/// segments, bending halfway between the two ports.
fn manhattan(
    path: &mut Vec<Point>,
    from: Point,
    from_dir: (f64, f64),
    to: Point,
    to_dir: (f64, f64),
) {
    // Work in a frame where the source faces +x: swap axes for vertical
    // sources, then map the generated points back.
    let horizontal = from_dir.0.abs() > 0.9;
    let fwd_side = |p: Point| if horizontal { (p.x, p.y) } else { (p.y, p.x) };
    let unmap = |a: f64, b: f64| {
        if horizontal {
            Point::new(a, b)
        } else {
            Point::new(b, a)
        }
    };
    let (fd, td_fwd, td_side) = if horizontal {
        (from_dir.0, to_dir.0, to_dir.1)
    } else {
        (from_dir.1, to_dir.1, to_dir.0)
    };
    let (fa, fs) = fwd_side(from);
    let (ta, ts) = fwd_side(to);

    let ahead = (ta - fa) * fd > 0.0;
    let facing = td_fwd * fd < -0.5;
    if ahead && facing {
        if (fs - ts).abs() >= 0.5 {
            let mid = (fa + ta) / 2.0;
            path.push(unmap(mid, fs));
            path.push(unmap(mid, ts));
        }
        return;
    }

    let p1 = fa + fd * STUB;
    // Entering from the side (e.g. a top port) with the port ahead: one bend.
    if td_side.abs() > 0.9 && (ta - p1) * fd >= 0.0 {
        path.push(unmap(ta, fs));
        return;
    }

    // Otherwise leave the source, detour along a lane clear of both ports,
    // and come back to the destination's approach point.
    let p2 = ta + td_fwd * STUB;
    let q2 = ts + td_side * STUB;
    let lane = fs.max(ts) + 3.0 * STUB;
    path.push(unmap(p1, fs));
    path.push(unmap(p1, lane));
    path.push(unmap(p2, lane));
    path.push(unmap(p2, q2));
}

fn longest_leading_segment(path: &[Point]) -> (Point, Point) {
    // Skip the stub from the outline to the anchor.
    let start = usize::from(path.len() > 2);
    path[start..]
        .windows(2)
        .take(2)
        .map(|w| (w[0], w[1]))
        .max_by(|a, b| {
            let la = (a.1.x - a.0.x).abs() + (a.1.y - a.0.y).abs();
            let lb = (b.1.x - b.0.x).abs() + (b.1.y - b.0.y).abs();
            la.total_cmp(&lb)
        })
        .unwrap_or((path[0], path[path.len() - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elbow_added_when_misaligned() {
        let mut p = vec![Point::new(0.0, 0.0), Point::new(20.0, 0.0)];
        finish_leg(&mut p, Point::new(50.0, 30.0), (-1.0, 0.0));
        assert_eq!(
            p,
            vec![
                Point::new(0.0, 0.0),
                Point::new(20.0, 0.0),
                Point::new(20.0, 30.0),
                Point::new(50.0, 30.0)
            ]
        );
    }

    #[test]
    fn aligned_leg_goes_straight() {
        let mut p = vec![Point::new(0.0, 10.0)];
        finish_leg(&mut p, Point::new(50.0, 10.0), (-1.0, 0.0));
        assert_eq!(p, vec![Point::new(0.0, 10.0), Point::new(50.0, 10.0)]);
    }

    #[test]
    fn manhattan_detours_around_same_facing_port() {
        // Source faces right; destination input is on the right edge of a
        // block further right (it also faces right). A straight line would
        // cross the destination block.
        let mut p = vec![Point::new(0.0, 0.0)];
        manhattan(
            &mut p,
            Point::new(0.0, 0.0),
            (1.0, 0.0),
            Point::new(100.0, 0.0),
            (1.0, 0.0),
        );
        assert_eq!(
            p,
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 30.0),
                Point::new(110.0, 30.0),
                Point::new(110.0, 0.0),
            ]
        );
    }

    #[test]
    fn manhattan_bends_midway() {
        let mut p = vec![Point::new(0.0, 0.0)];
        manhattan(
            &mut p,
            Point::new(0.0, 0.0),
            (1.0, 0.0),
            Point::new(40.0, 20.0),
            (-1.0, 0.0),
        );
        assert_eq!(
            p,
            vec![
                Point::new(0.0, 0.0),
                Point::new(20.0, 0.0),
                Point::new(20.0, 20.0)
            ]
        );
    }

    /// Each leaf wire names its own destination; shared trunks name none.
    #[test]
    fn wires_carry_their_own_destination() {
        let dst = |id: &str| {
            Some(Endpoint {
                block: id.into(),
                port: unlinked_model::PortRef {
                    kind: PortKind::In,
                    index: 1,
                },
            })
        };
        let line = Line {
            points: vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)],
            branches: vec![
                Branch {
                    points: vec![Point::new(20.0, 0.0)],
                    dst: dst("a"),
                    branches: vec![Branch {
                        points: vec![Point::new(20.0, 10.0)],
                        dst: dst("b"),
                        branches: vec![],
                    }],
                },
                Branch {
                    points: vec![Point::new(30.0, 0.0)],
                    dst: dst("c"),
                    branches: vec![],
                },
            ],
            ..Default::default()
        };
        let routed = route_line(&System::default(), &line);
        let dsts: Vec<_> = routed
            .wires
            .iter()
            .map(|w| w.dst.as_ref().map(|d| d.block.0.as_str()))
            .collect();
        assert_eq!(dsts, [Some("b"), Some("a"), Some("c"), None]);
    }
}
