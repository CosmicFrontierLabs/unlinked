//! Explicit leaf/trunk routing in absolute diagram coordinates.
use crate::edit::EditError;
use crate::{Branch, Endpoint, Point, PortKind, System};

pub(crate) fn validate_points(points: &[Point]) -> Result<(), EditError> {
    if points.len() > 4096
        || points
            .iter()
            .any(|p| !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > 1e9 || p.y.abs() > 1e9)
    {
        return Err(EditError::Invalid(
            "route needs at most 4096 finite vertices within +/-1e9".into(),
        ));
    }
    Ok(())
}

/// Locate one ordinary, fully attached line and optionally a terminal branch.
/// Path indices refer to branches below the selected line root.
pub fn route_location(
    sys: &System,
    endpoint: &Endpoint,
    trunk: bool,
) -> Result<(usize, Vec<usize>), EditError> {
    let mut matches = Vec::new();
    let mut budget = 500_000usize;
    for (root, line) in sys.lines.iter().enumerate() {
        budget = budget
            .checked_sub(1)
            .ok_or_else(|| EditError::Invalid("route traversal budget exceeded".into()))?;
        if trunk && line.src.as_ref() == Some(endpoint) {
            matches.push((root, vec![]));
        }
        if !trunk && line.dst.as_ref() == Some(endpoint) {
            matches.push((root, vec![]));
        }
        let mut pending: Vec<(&Branch, Vec<usize>)> = line
            .branches
            .iter()
            .enumerate()
            .map(|(i, b)| (b, vec![i]))
            .collect();
        while let Some((b, path)) = pending.pop() {
            budget = budget
                .checked_sub(1)
                .ok_or_else(|| EditError::Invalid("route traversal budget exceeded".into()))?;
            if path.len() > 128 {
                return Err(EditError::Invalid("route nesting exceeds 128".into()));
            }
            if !trunk && b.dst.as_ref() == Some(endpoint) {
                matches.push((root, path.clone()));
            }
            for (i, child) in b.branches.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                pending.push((child, p));
            }
        }
    }
    if matches.len() != 1 {
        return Err(EditError::Invalid(
            "route endpoint is missing or ambiguous".into(),
        ));
    }
    let (root, path) = matches.pop().unwrap();
    let line = &sys.lines[root];
    let src = line
        .src
        .as_ref()
        .ok_or_else(|| EditError::Invalid("detached routes cannot be edited".into()))?;
    if !matches!(src.port.kind, PortKind::Out | PortKind::State)
        || sys
            .lines
            .iter()
            .filter(|l| l.src.as_ref() == Some(src))
            .count()
            != 1
    {
        return Err(EditError::Invalid(
            "route requires a unique signal source".into(),
        ));
    }
    let mut blocks = std::collections::BTreeMap::new();
    for block in &sys.blocks {
        budget = budget
            .checked_sub(1)
            .ok_or_else(|| EditError::Invalid("route traversal budget exceeded".into()))?;
        blocks
            .entry(&block.id)
            .and_modify(|value| *value = None)
            .or_insert(Some(block));
    }
    let valid = |ep: &Endpoint| {
        blocks
            .get(&ep.block)
            .copied()
            .flatten()
            .is_some_and(|b| ep.port.index > 0 && ep.port.index <= b.ports.count(ep.port.kind))
    };
    if !valid(src) {
        return Err(EditError::Invalid("route source port is missing".into()));
    }
    let mut pending = vec![(line.dst.as_ref(), &line.branches)];
    while let Some((dst, branches)) = pending.pop() {
        if dst.is_none() && branches.is_empty() {
            return Err(EditError::Invalid(
                "dangling routes cannot be edited".into(),
            ));
        }
        if let Some(dst) = dst {
            if !matches!(
                dst.port.kind,
                PortKind::In
                    | PortKind::Enable
                    | PortKind::Trigger
                    | PortKind::IfAction
                    | PortKind::Reset
            ) || !valid(dst)
            {
                return Err(EditError::Invalid(
                    "route destination port is unsupported or missing".into(),
                ));
            }
        }
        pending.extend(branches.iter().map(|b| (b.dst.as_ref(), &b.branches)));
    }
    let mut children = &line.branches;
    for &i in &path {
        children = &children[i].branches;
    }
    if !trunk && !children.is_empty() {
        return Err(EditError::Invalid(
            "destination is not a terminal route; edit the trunk explicitly".into(),
        ));
    }
    Ok((root, path))
}

pub(crate) fn set_route(
    sys: &mut System,
    endpoint: &Endpoint,
    trunk: bool,
    points: &[Point],
) -> Result<(), EditError> {
    validate_points(points)?;
    let (root, path) = route_location(sys, endpoint, trunk)?;
    let line = &mut sys.lines[root];
    let mut target = &mut line.points;
    let mut children = &mut line.branches;
    for i in path {
        let b = &mut children[i];
        target = &mut b.points;
        children = &mut b.branches;
    }
    *target = points.to_vec();
    Ok(())
}
