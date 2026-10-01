//! Bounded structural diagnostics. These do not execute expressions or certify
//! simulation support. Targets use subsystem IDs, so renames do not move them.
//! Plain interface ports default to number one; bus elements use InterfaceData
//! and may share numbers within the same named port. Parent subsystem counts
//! and execution-dependent interface variants are not inferred here.
//! Masked/linked blocks use declared ports.
use crate::{catalog, BlockId, Endpoint, Model, Point, PortKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticTarget {
    Model,
    Block {
        system: Vec<BlockId>,
        id: BlockId,
        parameter: Option<String>,
    },
    /// Line root index in this snapshot, plus the affected endpoint if known.
    Line {
        system: Vec<BlockId>,
        root: usize,
        endpoint: Option<Endpoint>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub target: DiagnosticTarget,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuralReport {
    pub diagnostics: Vec<Diagnostic>,
    /// Work or report budget reached; absence of errors is inconclusive.
    pub truncated: bool,
}
impl StructuralReport {
    pub fn is_valid(&self) -> bool {
        !self.truncated
            && !self
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
    }
    fn emit(&mut self, severity: Severity, code: &str, target: DiagnosticTarget, message: &str) {
        if self.diagnostics.len() >= 1000 {
            self.truncated = true;
            return;
        }
        self.diagnostics.push(Diagnostic {
            severity,
            code: code.into(),
            target,
            message: message.into(),
        });
    }
}

fn finite(point: &Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

/// Checks catalog-resolved ports for native blocks, otherwise declared ports.
/// Unresolved inference is reported as a warning. Missing schema entries and
/// unsupported block types are deliberately not structural errors.
/// Bounded to 500,000 visited objects/vertices, 128 subsystem levels and 1,000
/// findings. Traversal is iterative, including branched lines.
pub fn validate_structure(model: &Model) -> StructuralReport {
    let mut report = StructuralReport::default();
    let mut budget = 500_000usize;
    let mut systems = vec![(&model.root, Vec::new())];
    macro_rules! spend {
        () => {
            if budget == 0 || report.truncated {
                report.truncated = true;
                return report;
            }
            budget -= 1;
        };
    }
    while let Some((system, path)) = systems.pop() {
        spend!();
        let mut blocks = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut effective_ports = BTreeMap::new();
        let mut numbered = BTreeMap::<&str, BTreeMap<u32, &crate::Block>>::new();
        for block in &system.blocks {
            spend!();
            let target = || DiagnosticTarget::Block {
                system: path.clone(),
                id: block.id.clone(),
                parameter: None,
            };
            if block.id.0.is_empty() || blocks.insert(&block.id, block).is_some() {
                report.emit(
                    Severity::Error,
                    "duplicate_or_empty_block_id",
                    target(),
                    "block IDs must be nonempty and unique within a system",
                );
            }
            if block.name.trim().is_empty() || !names.insert(&block.name) {
                report.emit(
                    Severity::Error,
                    "duplicate_or_empty_block_name",
                    target(),
                    "block names must be nonempty and unique within a system",
                );
            }
            let parameter_target = |parameter: &str| DiagnosticTarget::Block {
                system: path.clone(),
                id: block.id.clone(),
                parameter: Some(parameter.into()),
            };
            let native =
                block.mask.is_none() && block.library_source.is_none() && block.subsystem.is_none();
            if let Some(descriptor) = catalog::find(&block.block_type).filter(|_| native) {
                for parameter in descriptor.parameters {
                    spend!();
                    if let Some(value) = block.param(parameter.name) {
                        if let Err(message) = catalog::validate_parameter(parameter, value) {
                            report.emit(
                                Severity::Error,
                                "invalid_parameter",
                                parameter_target(parameter.name),
                                &message,
                            );
                        }
                    }
                }
                match descriptor.resolve_ports(&block.parameters) {
                    catalog::PortResolution::Known(ports) => { effective_ports.insert(&block.id, ports); }
                    catalog::PortResolution::Invalid { parameter, message } => report.emit(Severity::Error, "invalid_port_parameter", parameter_target(parameter), &message),
                    catalog::PortResolution::Unresolved { parameter } => report.emit(Severity::Warning, "unresolved_ports", parameter_target(parameter), "port count requires semantic expression resolution; checking only declared endpoints"),
                }
            }
            if native && matches!(block.block_type.as_str(), "Inport" | "Outport") {
                match catalog::interface_port_number(block) {
                    Ok(n) => {
                        if let Some(previous) = numbered
                            .entry(&block.block_type)
                            .or_default()
                            .insert(n, block)
                        {
                            if !catalog::share_interface(previous, block) {
                                report.emit(Severity::Error, "duplicate_port_number", parameter_target("Port"), "interface number is shared by different named ports or ordinary ports");
                            }
                        }
                    }
                    Err(message) => report.emit(
                        Severity::Error,
                        "invalid_port_number",
                        parameter_target("Port"),
                        &message,
                    ),
                }
            }
            let p = block.position;
            if ![p.left, p.top, p.right, p.bottom]
                .iter()
                .all(|x| x.is_finite())
                || p.right <= p.left
                || p.bottom <= p.top
            {
                report.emit(
                    Severity::Error,
                    "invalid_block_geometry",
                    target(),
                    "block rectangle must be finite and have positive size",
                );
            }
            if let Some(child) = &block.subsystem {
                if path.len() >= 128 {
                    report.truncated = true;
                    return report;
                }
                let mut next = path.clone();
                next.push(block.id.clone());
                systems.push((child.as_ref(), next));
            }
        }
        for (kind, numbers) in numbered {
            if numbers.keys().copied().ne(1..=numbers.len() as u32) {
                // Point at an affected interface block rather than an unrelated model error.
                if let Some(block) = system.blocks.iter().find(|b| b.block_type == kind) {
                    report.emit(
                        Severity::Error,
                        "port_number_gap",
                        DiagnosticTarget::Block {
                            system: path.clone(),
                            id: block.id.clone(),
                            parameter: Some("Port".into()),
                        },
                        "interface port numbers must be contiguous starting at one",
                    );
                }
            }
        }
        let mut driven = BTreeSet::new();
        for (root, line) in system.lines.iter().enumerate() {
            spend!();
            let target = |endpoint| DiagnosticTarget::Line {
                system: path.clone(),
                root,
                endpoint,
            };
            if line.src.is_none() {
                report.emit(
                    Severity::Warning,
                    "dangling_line_source",
                    target(None),
                    "line has no source",
                );
            }
            // Treat a destination appearing twice (even with the same source)
            // as ambiguous, rather than silently normalizing serialized roots.
            let mut portions = vec![(&line.points, line.dst.as_ref(), &line.branches)];
            let mut endpoints = Vec::new();
            if let Some(src) = &line.src {
                endpoints.push((src, true));
            }
            while let Some((points, dst, branches)) = portions.pop() {
                spend!();
                for point in points {
                    spend!();
                    if !finite(point) {
                        report.emit(
                            Severity::Error,
                            "invalid_line_geometry",
                            target(None),
                            "line points must be finite",
                        );
                        break;
                    }
                }
                if let Some(dst) = dst {
                    if !matches!(dst.port.kind, PortKind::LConn | PortKind::RConn)
                        && !driven.insert(dst)
                    {
                        report.emit(
                            Severity::Error,
                            "duplicate_driver",
                            target(Some(dst.clone())),
                            "input occurs in multiple connections",
                        );
                    }
                    endpoints.push((dst, false));
                } else if branches.is_empty() {
                    report.emit(
                        Severity::Warning,
                        "dangling_line_destination",
                        target(None),
                        "line leaf has no destination",
                    );
                }
                for branch in branches {
                    spend!();
                    portions.push((&branch.points, branch.dst.as_ref(), &branch.branches));
                }
            }
            for (endpoint, source) in endpoints {
                spend!();
                let Some(block) = blocks.get(&endpoint.block) else {
                    report.emit(
                        Severity::Error,
                        "missing_endpoint_block",
                        target(Some(endpoint.clone())),
                        "endpoint refers to a missing block",
                    );
                    continue;
                };
                if endpoint.port.index == 0
                    || endpoint.port.index
                        > effective_ports
                            .get(&block.id)
                            .unwrap_or(&block.ports)
                            .count(endpoint.port.kind)
                {
                    report.emit(
                        Severity::Error,
                        "invalid_endpoint_port",
                        target(Some(endpoint.clone())),
                        "endpoint exceeds the block's resolved or declared port count",
                    );
                }
                // Physical connector ports are intentionally not assigned an
                // ordinary signal direction here.
                let direction_ok = if source {
                    matches!(
                        endpoint.port.kind,
                        PortKind::Out | PortKind::State | PortKind::LConn | PortKind::RConn
                    )
                } else {
                    !matches!(endpoint.port.kind, PortKind::Out | PortKind::State)
                };
                if !direction_ok {
                    report.emit(
                        Severity::Error,
                        "invalid_endpoint_direction",
                        target(Some(endpoint.clone())),
                        "endpoint direction does not match source/destination position",
                    );
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Branch, Line, PortCounts, PortRef, Rect, SourceFormat, System};
    fn block(id: &str) -> Block {
        Block {
            id: id.into(),
            name: id.into(),
            block_type: "UnknownPreservedType".into(),
            position: Rect::new(0.0, 0.0, 40.0, 40.0),
            orientation: Default::default(),
            mirrored: false,
            ports: PortCounts {
                inputs: 1,
                outputs: 1,
                ..Default::default()
            },
            parameters: Default::default(),
            mask: None,
            library_source: None,
            subsystem: None,
            style: Default::default(),
            interface: None,
        }
    }
    fn model() -> Model {
        Model {
            name: "test".into(),
            source: SourceFormat::Mdl,
            simulink_version: None,
            config: Default::default(),
            root: System {
                blocks: vec![block("a"), block("b"), block("c")],
                ..Default::default()
            },
            workspace: Default::default(),
            charts: vec![],
        }
    }
    fn ep(id: &str, kind: PortKind, index: u32) -> Endpoint {
        Endpoint {
            block: id.into(),
            port: PortRef { kind, index },
        }
    }
    fn line() -> Line {
        Line {
            src: Some(ep("a", PortKind::Out, 1)),
            dst: Some(ep("b", PortKind::In, 1)),
            ..Default::default()
        }
    }
    #[test]
    fn unknown_blocks_and_disconnected_ports_are_preserved() {
        assert!(validate_structure(&model()).is_valid());
        let mut m = model();
        m.root.lines.push(line());
        m.root.lines[0].branches.push(Branch {
            dst: Some(ep("c", PortKind::In, 1)),
            ..Default::default()
        });
        assert!(validate_structure(&m).is_valid());
    }
    #[test]
    fn catches_duplicate_drivers_even_in_nested_branches() {
        let mut m = model();
        m.root.lines.push(line());
        m.root.lines[0].branches.push(Branch {
            branches: vec![Branch {
                dst: Some(ep("b", PortKind::In, 1)),
                ..Default::default()
            }],
            ..Default::default()
        });
        let r = validate_structure(&m);
        assert_eq!(
            r.diagnostics
                .iter()
                .filter(|d| d.code == "duplicate_driver")
                .count(),
            1
        );
        assert!(!r.is_valid());
    }
    #[test]
    fn invalid_endpoints_and_geometry_have_structured_targets() {
        let mut m = model();
        m.root.lines.push(line());
        m.root.lines[0].src = Some(ep("missing", PortKind::Out, 1));
        m.root.lines[0].dst = Some(ep("b", PortKind::Out, 0));
        m.root.blocks[0].position.left = f64::NAN;
        let r = validate_structure(&m);
        for code in [
            "missing_endpoint_block",
            "invalid_endpoint_port",
            "invalid_endpoint_direction",
            "invalid_block_geometry",
        ] {
            assert!(r.diagnostics.iter().any(|d| d.code == code));
        }
    }
    #[test]
    fn targets_use_subsystem_ids_and_reports_are_bounded() {
        let mut m = model();
        let mut child = System {
            blocks: vec![block("x"), block("x")],
            ..Default::default()
        };
        child.lines = vec![Line::default(); 1500];
        m.root.blocks[0].subsystem = Some(Box::new(child));
        let r = validate_structure(&m);
        assert!(r.truncated && !r.is_valid());
        assert_eq!(r.diagnostics.len(), 1000);
        assert!(
            matches!(&r.diagnostics[0].target, DiagnosticTarget::Block { system, .. } if system == &vec![BlockId::from("a")])
        );
    }
    #[test]
    fn parameter_edits_cannot_hide_removed_connected_ports() {
        let mut m = model();
        m.root.blocks[1].block_type = "Mux".into();
        m.root.blocks[1].ports.inputs = 3;
        m.root.blocks[1]
            .parameters
            .insert("Inputs".into(), "2".into());
        m.root.lines.push(Line {
            src: Some(ep("a", PortKind::Out, 1)),
            dst: Some(ep("b", PortKind::In, 3)),
            ..Default::default()
        });
        assert!(validate_structure(&m)
            .diagnostics
            .iter()
            .any(|d| d.code == "invalid_endpoint_port"));
        m.root.blocks[1]
            .parameters
            .insert("Inputs".into(), "**".into());
        // An operator-only Mux specification is malformed.
        assert!(validate_structure(&m)
            .diagnostics
            .iter()
            .any(|d| d.code == "invalid_port_parameter"));
    }
    #[test]
    fn port_numbers_are_allocated_contextually_and_validated() {
        let mut m = model();
        m.root.blocks[0].block_type = "Inport".into();
        m.root.blocks[0]
            .parameters
            .insert("Port".into(), "1".into());
        m.root.blocks[1].block_type = "Inport".into();
        m.root.blocks[1]
            .parameters
            .insert("Port".into(), "3".into());
        let parameters = catalog::find("Inport")
            .unwrap()
            .creation_parameters_in(&m.root)
            .unwrap();
        assert_eq!(parameters["Port"], "2");
        assert!(validate_structure(&m)
            .diagnostics
            .iter()
            .any(|d| d.code == "port_number_gap"));
        m.root.blocks[1]
            .parameters
            .insert("Port".into(), "1".into());
        assert!(validate_structure(&m)
            .diagnostics
            .iter()
            .any(|d| d.code == "duplicate_port_number"));
        m.root.blocks[1]
            .parameters
            .insert("Port".into(), "n".into());
        assert!(catalog::find("Inport")
            .unwrap()
            .creation_parameters_in(&m.root)
            .is_err());
    }
    #[test]
    fn schema_errors_target_the_parameter_but_unknown_parameters_survive() {
        let mut m = model();
        let block = &mut m.root.blocks[0];
        block.block_type = "Gain".into();
        block
            .parameters
            .insert("Multiplication".into(), "typo".into());
        block
            .parameters
            .insert("FutureOption".into(), "preserve".into());
        let r = validate_structure(&m);
        assert!(r.diagnostics.iter().any(|d| matches!(&d.target, DiagnosticTarget::Block { parameter: Some(p), .. } if p == "Multiplication")));
        assert!(!r
            .diagnostics
            .iter()
            .any(|d| d.message.contains("FutureOption")));
    }
    #[test]
    fn plain_default_one_and_bus_sharing_have_distinct_numbering() {
        let mut m = model();
        m.root.blocks[0].block_type = "Inport".into();
        assert_eq!(
            catalog::find("Inport")
                .unwrap()
                .creation_parameters_in(&m.root)
                .unwrap()["Port"],
            "2"
        );
        assert!(validate_structure(&m).is_valid());
        m.root.blocks[1].block_type = "Inport".into();
        assert!(!validate_structure(&m).is_valid());
        for b in &mut m.root.blocks[..2] {
            b.interface = Some(crate::PortInterface::from_properties(BTreeMap::from([
                ("PortNumber".into(), "1".into()),
                ("PortName".into(), "Sensors".into()),
                ("Element".into(), b.name.clone()),
            ])));
        }
        assert!(validate_structure(&m).is_valid());
        assert_eq!(
            catalog::find("Inport")
                .unwrap()
                .creation_parameters_in(&m.root)
                .unwrap()["Port"],
            "2"
        );
        m.root.blocks[1].interface.as_mut().unwrap().port_name = Some("Different".into());
        assert!(!validate_structure(&m).is_valid());
        assert!(catalog::find("Inport")
            .unwrap()
            .creation_parameters_in(&m.root)
            .is_err());
    }
}
