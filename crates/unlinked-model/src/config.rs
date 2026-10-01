//! Pure checks for stored simulation configuration; no MATLAB evaluation.
//!
//! Missing solver/start/stop/fixed-step values are unresolved, never silently
//! replaced with a solver or timing default. An absent SolverType is inferred
//! only from an explicit recognized solver. Missing RK45 tolerances use the
//! documented Unlinked run defaults (1e-6 relative, 1e-9 absolute); this does not
//! mutate or claim to reproduce Simulink defaults. RK45 observation spacing is
//! a separate run option, never inferred from FixedStep or MaxStep.
//!
//! Canonical solver names and types follow MathWorks' solver parameter reference:
//! <https://www.mathworks.com/help/simulink/gui/solver.html>.
use crate::validation::{Diagnostic, DiagnosticTarget, Severity};
use crate::SimConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SolverKind {
    FixedStep,
    VariableStep,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SolverDescriptor {
    /// Canonical value stored in a Simulink configuration.
    pub value: &'static str,
    pub label: &'static str,
    pub kind: SolverKind,
    /// Unlinked runtime solver name, or None when preservation is the only support.
    pub simulation_solver: Option<&'static str>,
}
macro_rules! solver {
    ($value:literal,$label:literal,$kind:ident,$runtime:expr) => {
        SolverDescriptor {
            value: $value,
            label: $label,
            kind: SolverKind::$kind,
            simulation_solver: $runtime,
        }
    };
}
pub const SOLVERS: &[SolverDescriptor] = &[
    solver!("ode1", "Euler (fixed step)", FixedStep, Some("euler")),
    solver!("ode4", "Runge–Kutta 4 (fixed step)", FixedStep, Some("rk4")),
    solver!(
        "ode45",
        "Dormand–Prince 5(4) (adaptive)",
        VariableStep,
        Some("rk45")
    ),
    solver!("FixedStepAuto", "Automatic (fixed step)", FixedStep, None),
    solver!(
        "FixedStepDiscrete",
        "Discrete (fixed step)",
        FixedStep,
        None
    ),
    solver!("ode2", "Heun (fixed step)", FixedStep, None),
    solver!("ode3", "Bogacki–Shampine (fixed step)", FixedStep, None),
    solver!("ode5", "Dormand–Prince (fixed step)", FixedStep, None),
    solver!("ode8", "Dormand–Prince 8 (fixed step)", FixedStep, None),
    solver!("ode14x", "Extrapolation (fixed step)", FixedStep, None),
    solver!("ode1be", "Backward Euler (fixed step)", FixedStep, None),
    solver!(
        "VariableStepAuto",
        "Automatic (variable step)",
        VariableStep,
        None
    ),
    solver!(
        "VariableStepDiscrete",
        "Discrete (variable step)",
        VariableStep,
        None
    ),
    solver!("ode23", "Bogacki–Shampine (adaptive)", VariableStep, None),
    solver!("ode113", "Adams (adaptive)", VariableStep, None),
    solver!("ode15s", "NDF (stiff)", VariableStep, None),
    solver!("ode23s", "Modified Rosenbrock (stiff)", VariableStep, None),
    solver!(
        "ode23t",
        "Trapezoidal (moderately stiff)",
        VariableStep,
        None
    ),
    solver!("ode23tb", "TR-BDF2 (stiff)", VariableStep, None),
    solver!("odeN", "Nonadaptive Runge–Kutta", VariableStep, None),
    solver!(
        "daessc",
        "Differential-algebraic equations",
        VariableStep,
        None
    ),
];
/// Recognize runtime aliases without altering the stored value.
pub fn solver_descriptor(value: &str) -> Option<&'static SolverDescriptor> {
    let canonical = match value.trim() {
        "euler" => "ode1",
        "rk4" => "ode4",
        "rk45" => "ode45",
        other => other,
    };
    SOLVERS.iter().find(|solver| solver.value == canonical)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReport {
    pub diagnostics: Vec<Diagnostic>,
    /// Only compatibility of the inspected configuration fields, not a complete
    /// run request or certification of model/block support. RK45 still needs a
    /// separate explicit observation interval from the caller. Unresolved active
    /// settings and unsupported solver options make this false.
    pub simulation_supported: bool,
    /// At least one value needs expression evaluation, automatic selection, or
    /// an explicit caller choice. Preserving that value is still permitted.
    pub unresolved: bool,
}
impl ConfigReport {
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
    fn emit(&mut self, severity: Severity, code: &str, parameter: &str, message: &str) {
        if severity == Severity::Error {
            self.simulation_supported = false;
        }
        self.diagnostics.push(Diagnostic {
            severity,
            code: code.into(),
            target: DiagnosticTarget::Config {
                parameter: parameter.into(),
            },
            message: message.into(),
        });
    }
    fn unresolved(&mut self, parameter: &str, message: &str, active: bool) {
        self.unresolved = true;
        if active {
            self.simulation_supported = false;
        }
        self.emit(Severity::Warning, "config_unresolved", parameter, message);
    }
    fn unsupported(&mut self, parameter: &str, message: &str) {
        self.simulation_supported = false;
        self.emit(Severity::Warning, "config_unsupported", parameter, message);
    }
}
/// Typed fields are authoritative; raw-only configurations remain checkable.
/// Conflicting mirror values are reported, never silently selected.
fn value<'a>(
    config: &'a SimConfig,
    typed: Option<&'a str>,
    key: &str,
    report: &mut ConfigReport,
) -> Option<&'a str> {
    let raw = config.raw.get(key).map(String::as_str);
    if let (Some(a), Some(b)) = (typed, raw) {
        if a.trim() != b.trim() {
            report.emit(
                Severity::Error,
                "config_conflict",
                key,
                "Typed and raw configuration values disagree.",
            );
        }
    }
    typed.or(raw)
}
#[derive(Clone, Copy)]
enum Range {
    Finite,
    Positive,
    Nonnegative,
    RelativeTolerance,
}
fn number(
    report: &mut ConfigReport,
    key: &str,
    value: Option<&str>,
    range: Range,
    required: bool,
    active: bool,
    auto: bool,
) -> Option<f64> {
    let Some(raw) = value else {
        if required {
            report.unresolved(
                key,
                "No stored value; choose an explicit run setting.",
                active,
            );
        }
        return None;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        report.emit(
            Severity::Error,
            "config_empty",
            key,
            "A stored configuration value cannot be empty.",
        );
        return None;
    }
    if raw.len() > 64 * 1024 {
        report.emit(
            Severity::Error,
            "config_too_large",
            key,
            "Configuration expression exceeds 64 KiB.",
        );
        return None;
    }
    if auto && raw.eq_ignore_ascii_case("auto") {
        report.unresolved(
            key,
            "Automatic selection requires an explicit run value; the stored setting is preserved.",
            active,
        );
        return None;
    }
    let parsed = raw.parse::<f64>();
    let Ok(n) = parsed else {
        report.unresolved(
            key,
            "MATLAB expression preserved without evaluation; resolve it before running.",
            active,
        );
        return None;
    };
    if !n.is_finite() {
        if key == "StopTime" && n == f64::INFINITY {
            report.unsupported(key,"An unlimited Simulink stop time is preserved, but Unlinked requires a finite run stop.");
        } else {
            report.emit(
                Severity::Error,
                "config_nonfinite",
                key,
                "A finite scalar value is required.",
            );
        }
        return None;
    }
    let valid = match range {
        Range::Finite => true,
        Range::Positive => n > 0.,
        Range::Nonnegative => n >= 0.,
        Range::RelativeTolerance => n > 0. && n <= 1.,
    };
    if !valid {
        let message = match range {
            Range::Positive => "Value must be positive.",
            Range::Nonnegative => "Value must be nonnegative.",
            Range::RelativeTolerance => "Unlinked relative tolerance must satisfy 0 < RelTol <= 1.",
            Range::Finite => unreachable!(),
        };
        report.emit(Severity::Error, "config_range", key, message);
        return None;
    }
    Some(n)
}

pub fn validate_config(config: &SimConfig) -> ConfigReport {
    let mut report = ConfigReport {
        simulation_supported: true,
        ..Default::default()
    };
    let solver = value(config, config.solver.as_deref(), "Solver", &mut report);
    let descriptor = match solver {
        None => {
            report.unresolved(
                "Solver",
                "No stored solver; choose one explicitly rather than using a fallback.",
                true,
            );
            None
        }
        Some(s) if s.trim().is_empty() => {
            report.emit(
                Severity::Error,
                "config_empty",
                "Solver",
                "A stored solver name cannot be empty.",
            );
            None
        }
        Some(s) => {
            match solver_descriptor(s) {
                Some(d) => {
                    if d.simulation_solver.is_none() {
                        report.unsupported(
                            "Solver",
                            "This Simulink solver is preserved but not implemented by Unlinked.",
                        );
                    }
                    Some(d)
                }
                None => {
                    report.unsupported("Solver","Unrecognized solver name is preserved; no runtime solver will be substituted.");
                    None
                }
            }
        }
    };
    let kind = descriptor.map(|d| d.kind);
    if let Some(s) = config.raw.get("SolverType") {
        let explicit = match s.trim() {
            "Fixed-step" => Some(SolverKind::FixedStep),
            "Variable-step" => Some(SolverKind::VariableStep),
            _ => None,
        };
        if explicit.is_none() {
            report.emit(
                Severity::Error,
                "config_solver_type",
                "SolverType",
                "SolverType must be Fixed-step or Variable-step.",
            );
        } else if kind.is_some() && explicit != kind {
            report.emit(
                Severity::Error,
                "config_solver_type_mismatch",
                "SolverType",
                "SolverType conflicts with the selected solver's integration method.",
            );
        }
    }
    let start = value(
        config,
        config.start_time.as_deref(),
        "StartTime",
        &mut report,
    );
    let stop = value(config, config.stop_time.as_deref(), "StopTime", &mut report);
    let step = value(
        config,
        config.fixed_step.as_deref(),
        "FixedStep",
        &mut report,
    );
    let start = number(
        &mut report,
        "StartTime",
        start,
        Range::Finite,
        true,
        true,
        false,
    );
    let stop = number(
        &mut report,
        "StopTime",
        stop,
        Range::Finite,
        true,
        true,
        false,
    );
    if let (Some(start), Some(stop)) = (start, stop) {
        if stop < start {
            report.emit(
                Severity::Error,
                "config_time_range",
                "StopTime",
                "StopTime must be greater than or equal to StartTime.",
            );
        }
    }
    let fixed = kind == Some(SolverKind::FixedStep);
    number(
        &mut report,
        "FixedStep",
        step,
        Range::Positive,
        fixed,
        fixed,
        true,
    );
    let variable = kind == Some(SolverKind::VariableStep);
    let rel = config.raw.get("RelTol").map(String::as_str);
    let abs = config.raw.get("AbsTol").map(String::as_str);
    number(
        &mut report,
        "RelTol",
        rel,
        Range::RelativeTolerance,
        false,
        variable,
        false,
    );
    number(
        &mut report,
        "AbsTol",
        abs,
        Range::Positive,
        false,
        variable,
        true,
    );
    if descriptor.is_some_and(|d| d.value == "ode45") {
        report.emit(Severity::Warning,"config_output_sampling","FixedStep","RK45 uses a separate output sampling interval supplied with the run; FixedStep and MaxStep do not define that interval.");
        if rel.is_none() || abs.is_none() {
            report.emit(Severity::Warning,"config_run_defaults","AbsTol","Missing RK45 tolerances use Unlinked run defaults: relative 1e-6 and absolute 1e-9; stored configuration is unchanged.");
        }
    }
    let max = number(
        &mut report,
        "MaxStep",
        config.raw.get("MaxStep").map(String::as_str),
        Range::Positive,
        false,
        variable,
        true,
    );
    let min = number(
        &mut report,
        "MinStep",
        config.raw.get("MinStep").map(String::as_str),
        Range::Nonnegative,
        false,
        variable,
        true,
    );
    let initial = number(
        &mut report,
        "InitialStep",
        config.raw.get("InitialStep").map(String::as_str),
        Range::Positive,
        false,
        variable,
        true,
    );
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            report.emit(
                Severity::Error,
                "config_step_range",
                "MinStep",
                "MinStep cannot exceed MaxStep.",
            );
        }
    }
    if let (Some(initial), Some(max)) = (initial, max) {
        if initial > max {
            report.emit(
                Severity::Error,
                "config_step_range",
                "InitialStep",
                "InitialStep cannot exceed MaxStep.",
            );
        }
    }
    if variable {
        for key in ["MaxStep", "MinStep", "InitialStep"] {
            if config.raw.contains_key(key) {
                report.unsupported(key,"This stored internal-step constraint is not an Unlinked run option; it cannot be silently applied or replaced by the output sampling interval.");
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixed() -> SimConfig {
        SimConfig {
            solver: Some("ode4".into()),
            start_time: Some("0".into()),
            stop_time: Some("10".into()),
            fixed_step: Some("0.01".into()),
            raw: Default::default(),
        }
    }
    fn has(report: &ConfigReport, code: &str, key: &str) -> bool {
        report.diagnostics.iter().any(|d| {
            d.code == code
                && d.target
                    == DiagnosticTarget::Config {
                        parameter: key.into(),
                    }
        })
    }
    #[test]
    fn supported_solver_names_and_aliases_have_exact_runtime_mappings() {
        for (stored, runtime, kind) in [
            ("ode1", "euler", SolverKind::FixedStep),
            ("euler", "euler", SolverKind::FixedStep),
            ("ode4", "rk4", SolverKind::FixedStep),
            ("rk4", "rk4", SolverKind::FixedStep),
            ("ode45", "rk45", SolverKind::VariableStep),
            ("rk45", "rk45", SolverKind::VariableStep),
        ] {
            let descriptor = solver_descriptor(stored).unwrap();
            assert_eq!(descriptor.simulation_solver, Some(runtime));
            assert_eq!(descriptor.kind, kind);
            let mut config = fixed();
            config.solver = Some(stored.into());
            let before = config.clone();
            let report = validate_config(&config);
            assert!(report.is_valid());
            assert!(report.simulation_supported);
            assert!(!report.unresolved);
            assert_eq!(before, config);
        }
        assert!(SOLVERS
            .iter()
            .all(|a| SOLVERS.iter().filter(|b| a.value == b.value).count() == 1));
    }
    #[test]
    fn other_simulink_solvers_and_future_names_are_preservable_not_substituted() {
        for stored in [
            "ode2",
            "ode3",
            "ode5",
            "ode8",
            "ode14x",
            "ode1be",
            "ode113",
            "ode15s",
            "ode23s",
            "ode23t",
            "ode23tb",
            "odeN",
            "daessc",
            "VariableStepAuto",
            "FixedStepDiscrete",
            "odeFuture",
        ] {
            let mut config = fixed();
            config.solver = Some(stored.into());
            let report = validate_config(&config);
            assert!(report.is_valid(), "{stored}");
            assert!(!report.simulation_supported);
            assert!(has(&report, "config_unsupported", "Solver"));
        }
    }
    #[test]
    fn absent_required_settings_do_not_invent_runtime_defaults() {
        let report = validate_config(&SimConfig::default());
        assert!(report.is_valid());
        assert!(!report.simulation_supported);
        assert!(report.unresolved);
        for key in ["Solver", "StartTime", "StopTime"] {
            assert!(has(&report, "config_unresolved", key));
        }
        let mut config = fixed();
        config.fixed_step = None;
        assert!(has(
            &validate_config(&config),
            "config_unresolved",
            "FixedStep"
        ));
    }
    #[test]
    fn expressions_and_automatic_steps_are_unresolved_without_evaluation() {
        let mut config = fixed();
        config.fixed_step = Some("1/4000".into());
        config.stop_time = Some("Tfinal".into());
        let report = validate_config(&config);
        assert!(report.is_valid() && report.unresolved && !report.simulation_supported);
        assert!(has(&report, "config_unresolved", "FixedStep"));
        assert!(has(&report, "config_unresolved", "StopTime"));
        config.fixed_step = Some("auto".into());
        assert!(has(
            &validate_config(&config),
            "config_unresolved",
            "FixedStep"
        ));
    }
    #[test]
    fn literal_ranges_nonfinite_and_conflicting_mirrors_are_reported() {
        for step in ["0", "-0.01", "NaN", "Inf", "1e999", ""] {
            let mut config = fixed();
            config.fixed_step = Some(step.into());
            assert!(!validate_config(&config).is_valid(), "{step}");
        }
        let mut config = fixed();
        config.start_time = Some("20".into());
        assert!(has(
            &validate_config(&config),
            "config_time_range",
            "StopTime"
        ));
        config.start_time = Some("0".into());
        config.raw.insert("StartTime".into(), "1".into());
        assert!(has(
            &validate_config(&config),
            "config_conflict",
            "StartTime"
        ));
        config.raw.clear();
        config.stop_time = Some("Inf".into());
        let report = validate_config(&config);
        assert!(report.is_valid());
        assert!(!report.simulation_supported);
        assert!(has(&report, "config_unsupported", "StopTime"));
    }
    #[test]
    fn solver_type_matches_algorithm_not_observation_spacing() {
        let mut config = fixed();
        config
            .raw
            .insert("SolverType".into(), "Variable-step".into());
        assert!(has(
            &validate_config(&config),
            "config_solver_type_mismatch",
            "SolverType"
        ));
        config.solver = Some("ode45".into());
        let report = validate_config(&config);
        assert!(report.is_valid() && report.simulation_supported);
        assert!(has(&report, "config_output_sampling", "FixedStep"));
        assert!(has(&report, "config_run_defaults", "AbsTol"));
        config.fixed_step = Some("auto".into());
        let report = validate_config(&config);
        assert!(report.simulation_supported);
        assert!(report.unresolved);
        config.raw.insert("SolverType".into(), "bogus".into());
        assert!(!validate_config(&config).is_valid());
    }
    #[test]
    fn tolerance_limits_and_internal_step_constraints_are_not_silently_ignored() {
        let mut config = fixed();
        config.solver = Some("ode45".into());
        for (key, value) in [
            ("RelTol", "0"),
            ("RelTol", "2"),
            ("AbsTol", "-1"),
            ("AbsTol", "NaN"),
        ] {
            config.raw.clear();
            config.raw.insert(key.into(), value.into());
            assert!(!validate_config(&config).is_valid());
        }
        config.raw.clear();
        config.raw.insert("AbsTol".into(), "auto".into());
        let report = validate_config(&config);
        assert!(report.is_valid() && report.unresolved && !report.simulation_supported);
        config.raw.clear();
        config.raw.insert("MaxStep".into(), "0.1".into());
        config.raw.insert("MinStep".into(), "0.2".into());
        let report = validate_config(&config);
        assert!(has(&report, "config_step_range", "MinStep"));
        assert!(has(&report, "config_unsupported", "MaxStep"));
        config.raw.insert("MinStep".into(), "0".into());
        config.raw.insert("InitialStep".into(), "0.3".into());
        assert!(has(
            &validate_config(&config),
            "config_step_range",
            "InitialStep"
        ));
    }
    #[test]
    fn raw_only_values_and_serialized_config_targets_work() {
        let mut config = SimConfig::default();
        config.raw.extend(
            [
                ("Solver", "ode1"),
                ("StartTime", "0"),
                ("StopTime", "1"),
                ("FixedStep", "1e-3"),
            ]
            .map(|(k, v)| (k.into(), v.into())),
        );
        assert!(validate_config(&config).simulation_supported);
        let report = validate_config(&SimConfig::default());
        let encoded = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serde_json::from_str::<ConfigReport>(&encoded).unwrap(),
            report
        );
    }
}
