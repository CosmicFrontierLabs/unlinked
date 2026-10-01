//! Editor diagnostics, without simulations or callbacks.
//!
//! Static checks never evaluate MATLAB. Compilation evaluates bounded, pure
//! parameter/input expressions, not initialization scripts. Run compilation in
//! a disposable web worker and terminate superseded workers: compiler work is
//! bounded but not cooperatively cancellable. Neither a clean static report nor
//! successful compilation certifies runtime behavior or numerical accuracy.
use crate::{Error, Options};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use unlinked_model::validation::{validate_structure, Diagnostic, DiagnosticTarget, Severity};
use unlinked_model::{catalog, Block, BlockId, Model};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckMode {
    #[default]
    Static,
    Compile,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticContext {
    pub mode: CheckMode,
    /// Required for Compile. Explicit run settings, not imported config defaults.
    pub options: Option<Options>,
    /// Explicit pure numeric expressions overriding the model workspace. No scripts.
    pub workspace: BTreeMap<String, String>,
    /// Constant root inputs, keyed by original root Inport IDs.
    pub inputs: BTreeMap<BlockId, String>,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SimulationCheck {
    #[default]
    NotChecked,
    /// The compiler accepted this snapshot and explicit context. No simulation ran.
    Compiled,
    Rejected,
    /// Preflight/structural limits or missing explicit settings prevented checking.
    Incomplete,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub diagnostics: Vec<Diagnostic>,
    pub truncated: bool,
    pub warnings_omitted: bool,
    pub simulation: SimulationCheck,
}
impl DiagnosticReport {
    fn emit(&mut self, severity: Severity, code: &str, target: DiagnosticTarget, message: String) {
        let limit = if severity == Severity::Warning {
            100
        } else {
            1000
        };
        if self
            .diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
            >= limit
            || self.diagnostics.len() >= 1000
        {
            if severity == Severity::Warning {
                self.warnings_omitted = true;
            } else {
                self.truncated = true;
            }
            return;
        }
        self.diagnostics.push(Diagnostic {
            severity,
            code: code.into(),
            target,
            message,
        });
    }
}

struct ByteLimit(usize);
impl Write for ByteLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("diagnostic text budget"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Cheap iterative snapshot check, also used before UI-thread serialization.
/// Bounds diagnostic target/path amplification before any target clones.
pub fn snapshot_shape_bounded(model: &Model) -> bool {
    let mut nodes = 25_000usize;
    let mut blocks = 5000usize;
    let mut pending = vec![(&model.root, 0usize, 0usize)];
    while let Some((system, depth, ancestor_bytes)) = pending.pop() {
        if depth > 32 {
            return false;
        }
        let Some(left) = blocks.checked_sub(system.blocks.len()) else {
            return false;
        };
        blocks = left;
        let Some(left) = nodes.checked_sub(system.lines.len()) else {
            return false;
        };
        nodes = left;
        for b in &system.blocks {
            if b.id.0.len() > 1024 {
                return false;
            }
            // Escaped slash paths can be twice as long as their stored names.
            let own_bytes =
                b.id.0
                    .len()
                    .saturating_add(b.id.0.bytes().filter(|b| *b == b'/').count())
                    .saturating_add(b.name.len())
                    .saturating_add(b.name.bytes().filter(|b| *b == b'/').count())
                    .saturating_add(2);
            let path_bytes = ancestor_bytes.saturating_add(own_bytes);
            if path_bytes > 4096 {
                return false;
            }
            if let Some(child) = b.subsystem.as_deref() {
                pending.push((child, depth + 1, path_bytes));
            }
        }
        let endpoint_allowed = |ep: &Option<unlinked_model::Endpoint>| {
            ep.as_ref().is_none_or(|ep| ep.block.0.len() <= 1024)
        };
        for line in &system.lines {
            if !endpoint_allowed(&line.src) || !endpoint_allowed(&line.dst) {
                return false;
            }
            let mut branches = vec![];
            let Some(left) = nodes.checked_sub(line.branches.len() + line.points.len()) else {
                return false;
            };
            nodes = left;
            branches.extend(line.branches.iter().map(|b| (b, 0usize)));
            while let Some((branch, depth)) = branches.pop() {
                if !endpoint_allowed(&branch.dst) {
                    return false;
                }
                if depth > 32 {
                    return false;
                }
                let Some(left) = nodes.checked_sub(branch.branches.len() + branch.points.len())
                else {
                    return false;
                };
                nodes = left;
                branches.extend(branch.branches.iter().map(|b| (b, depth + 1)));
            }
        }
    }
    true
}
/// Limits opaque data before cloning/compilation, after bounded shape traversal.
fn bounded(model: &Model, context: &DiagnosticContext) -> bool {
    if !snapshot_shape_bounded(model) || context.inputs.keys().any(|id| id.0.len() > 1024) {
        return false;
    }
    let mut bytes = ByteLimit(2 * 1024 * 1024);
    serde_json::to_writer(&mut bytes, model).is_ok()
        && serde_json::to_writer(&mut bytes, context).is_ok()
}
fn target(path: &[BlockId], block: &Block, parameter: Option<&str>) -> DiagnosticTarget {
    DiagnosticTarget::Block {
        system: path.to_vec(),
        id: block.id.clone(),
        parameter: parameter.map(str::to_owned),
    }
}
fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn remember(
    map: &mut BTreeMap<String, Option<DiagnosticTarget>>,
    key: String,
    value: DiagnosticTarget,
) {
    map.entry(key)
        .and_modify(|old| {
            if old.as_ref() != Some(&value) {
                *old = None;
            }
        })
        .or_insert(Some(value));
}
fn error_target(block: &str, map: &BTreeMap<String, Option<DiagnosticTarget>>) -> DiagnosticTarget {
    // Exact original/flattened IDs only. Generated lowering IDs and ambiguous
    // original IDs fall back to Model rather than highlighting the wrong block.
    map.get(block)
        .and_then(Clone::clone)
        .unwrap_or(DiagnosticTarget::Model)
}

/// Bounded checks for a single immutable snapshot. Render reports only if that
/// snapshot/generation is still current; line root indices belong to it.
pub fn diagnose(model: &Model, context: &DiagnosticContext) -> DiagnosticReport {
    let mut report = DiagnosticReport::default();
    if !bounded(model, context) {
        report.truncated = true;
        report.simulation = SimulationCheck::Incomplete;
        report.emit(Severity::Warning, "diagnostic_budget", DiagnosticTarget::Model,
            "Diagnostics stopped: limit is 5,000 blocks, 25,000 line/branch/vertex objects, depth 32, 1 KiB block/endpoint IDs, 4 KiB accumulated ID/name paths and 2 MiB serialized model/context text.".into());
        return report;
    }
    let structural = validate_structure(model);
    let structural_ok = structural.is_valid();
    report.diagnostics = structural.diagnostics;
    report.truncated = structural.truncated;
    report.warnings_omitted = structural.warnings_omitted;
    for diagnostic in unlinked_model::config::validate_config(&model.config).diagnostics {
        report.emit(
            diagnostic.severity,
            &diagnostic.code,
            diagnostic.target,
            diagnostic.message,
        );
    }
    let mut ids = BTreeMap::new();
    let mut systems = vec![(&model.root, Vec::<BlockId>::new(), String::new())];
    while let Some((system, path, prefix)) = systems.pop() {
        for block in &system.blocks {
            let location = target(&path, block, None);
            remember(&mut ids, block.id.0.clone(), location.clone());
            let flat = format!("{prefix}{}", block.id.0.replace('/', "//"));
            remember(&mut ids, flat.clone(), location.clone());
            if block.mask.is_some() || block.library_source.is_some() {
                report.emit(Severity::Warning, "simulation_link_or_mask", location.clone(),
                    "Masks and library links require supported explicit lowering before simulation.".into());
            } else if block.subsystem.is_none()
                && !crate::vector::supported_native_type(&block.block_type)
            {
                report.emit(
                    Severity::Warning,
                    "simulation_block_type",
                    location.clone(),
                    format!(
                        "Block type {} is not supported by the simulator.",
                        block.block_type
                    ),
                );
            }
            if path.is_empty()
                && block.block_type == "Inport"
                && !context.inputs.contains_key(&block.id)
            {
                report.emit(
                    Severity::Warning,
                    "simulation_input_missing",
                    location.clone(),
                    "Provide an explicit root input binding to compile this model.".into(),
                );
            }
            if block
                .param("Commented")
                .is_some_and(|v| !matches!(v, "" | "off"))
            {
                report.emit(
                    Severity::Warning,
                    "simulation_commented",
                    location.clone(),
                    "Commented and pass-through blocks are not supported by the simulator.".into(),
                );
            }
            if block.mask.is_none() && block.library_source.is_none() {
                for (key, allowed) in crate::import::literal_requirements(&block.block_type) {
                    if let Some(value) = block.param(key).filter(|v| !allowed.contains(v)) {
                        report.emit(Severity::Warning, "simulation_mode", target(&path, block, Some(key)),
                            format!("The simulator does not support {key}={value}. Supported values: {}.", allowed.join(", ")));
                    }
                }
            }
            if let Some(descriptor) = catalog::find(&block.block_type) {
                for p in descriptor.parameters {
                    if !matches!(
                        p.kind,
                        catalog::ParameterKind::Expression
                            | catalog::ParameterKind::IntegerExpression
                    ) {
                        continue;
                    }
                    let Some(value) = block.param(p.name).map(str::trim) else {
                        continue;
                    };
                    if identifier(value)
                        && !["pi", "Inf", "inf", "NaN", "nan", "true", "false"].contains(&value)
                        && !model.workspace.contains_key(value)
                        && !context.workspace.contains_key(value)
                    {
                        report.emit(Severity::Warning, "workspace_binding_missing", target(&path, block, Some(p.name)),
                            format!("{value} is not in the supplied workspace. Supply its value or an explicitly evaluated init-script context; diagnostics never run callbacks."));
                    }
                }
            }
            // MATLAB Function backing blocks are implementation details lowered
            // as one chart. Do not label their internal S-Functions unsupported.
            if block.stateflow_type().is_some() {
                continue;
            }
            if let Some(child) = block.subsystem.as_deref() {
                let mut child_path = path.clone();
                child_path.push(block.id.clone());
                systems.push((child, child_path, format!("{flat}/")));
            }
        }
    }
    if context.mode == CheckMode::Static {
        return report;
    }
    if !structural_ok || report.truncated {
        report.simulation = SimulationCheck::Incomplete;
        return report;
    }
    let Some(options) = &context.options else {
        report.simulation = SimulationCheck::Incomplete;
        report.emit(Severity::Warning, "simulation_options_missing", DiagnosticTarget::Model,
            "Compile checking requires explicit run settings; imported solver settings are not substituted.".into());
        return report;
    };
    if let Err(error) = crate::sample_count(options) {
        report.simulation = SimulationCheck::Rejected;
        report.emit(
            Severity::Error,
            "simulation_options",
            DiagnosticTarget::Model,
            error.to_string(),
        );
        return report;
    }
    let mut model = model.clone();
    model.workspace.extend(context.workspace.clone());
    let result = if context.inputs.is_empty() {
        crate::compile(&model, options)
    } else {
        crate::evaluate_inputs(&model, &context.inputs)
            .and_then(|inputs| crate::compile_with_inputs(&model, options, &inputs))
    };
    match result {
        Ok(_) => report.simulation = SimulationCheck::Compiled,
        Err(error) => {
            report.simulation = SimulationCheck::Rejected;
            let (code, location) = match &error {
                Error::Block { block, .. } => ("simulation_compile", error_target(block, &ids)),
                Error::Parameter(_, _) => ("simulation_workspace", DiagnosticTarget::Model),
                Error::AlgebraicLoop(_) => ("simulation_algebraic_loop", DiagnosticTarget::Model),
                Error::Connection(_) => ("simulation_connection", DiagnosticTarget::Model),
                Error::Options(_) => ("simulation_options", DiagnosticTarget::Model),
                Error::Cancelled => ("simulation_cancelled", DiagnosticTarget::Model),
            };
            report.emit(Severity::Error, code, location, error.to_string());
        }
    }
    report
}
