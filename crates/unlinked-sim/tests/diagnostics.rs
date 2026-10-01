use unlinked_model::validation::{DiagnosticTarget, Severity};
use unlinked_sim::diagnose::{diagnose, CheckMode, DiagnosticContext, SimulationCheck};
use unlinked_sim::Options;

fn model(gain: &str) -> unlinked_model::Model {
    let text = format!(
        r#"Model {{
 Name test
 Solver ode4
 StartTime 0
 StopTime 1
 FixedStep 0.1
 InitFcn "K=99;"
 System {{
 Block {{
 BlockType Constant
 Name source
 SID 1
 Position [0,0,30,30]
 Value 2
 }}
 Block {{
 BlockType Gain
 Name gain
 SID 2
 Position [100,0,130,30]
 Gain "{gain}"
 }}
 Line {{
 SrcBlock source
 SrcPort 1
 DstBlock gain
 DstPort 1
 }}
 }}
}}"#
    );
    unlinked_import::import("test.mdl", text.as_bytes()).unwrap()
}
fn compile_context() -> DiagnosticContext {
    DiagnosticContext {
        mode: CheckMode::Compile,
        options: Some(Options::default()),
        ..Default::default()
    }
}
#[test]
fn static_is_not_execution_certification_and_never_runs_init_callbacks() {
    let m = model("K");
    let report = diagnose(&m, &DiagnosticContext::default());
    assert_eq!(report.simulation, SimulationCheck::NotChecked);
    assert!(report.diagnostics.iter().any(|d| d.code == "workspace_binding_missing"
        && matches!(&d.target,DiagnosticTarget::Block{id,parameter:Some(p),..} if id.0=="2" && p=="Gain")));
    let report = diagnose(&m, &compile_context());
    assert_eq!(report.simulation, SimulationCheck::Rejected);
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error
            && matches!(&d.target,DiagnosticTarget::Block{id,..} if id.0=="2")));
}
#[test]
fn explicit_workspace_compiles_without_mutating_model() {
    let m = model("K");
    let before = m.clone();
    let mut context = compile_context();
    context.workspace.insert("K".into(), "3".into());
    let report = diagnose(&m, &context);
    assert_eq!(
        report.simulation,
        SimulationCheck::Compiled,
        "{:?}",
        report.diagnostics
    );
    assert_eq!(m, before);
    assert!(!report
        .diagnostics
        .iter()
        .any(|d| d.code == "workspace_binding_missing"));
}
#[test]
fn explicit_options_are_required_even_for_imported_supported_solver() {
    let report = diagnose(
        &model("1"),
        &DiagnosticContext {
            mode: CheckMode::Compile,
            ..Default::default()
        },
    );
    assert_eq!(report.simulation, SimulationCheck::Incomplete);
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "simulation_options_missing"));
}
#[test]
fn mode_limits_share_the_actual_compiler_rules() {
    let mut m = model("1");
    m.root.blocks[1].block_type = "Integrator".into();
    m.root.blocks[1]
        .parameters
        .insert("WrapState".into(), "on".into());
    let report = diagnose(&m, &DiagnosticContext::default());
    assert!(report.diagnostics.iter().any(|d| d.code=="simulation_mode"
        && matches!(&d.target,DiagnosticTarget::Block{parameter:Some(p),..} if p=="WrapState")));
    assert_eq!(
        diagnose(&m, &compile_context()).simulation,
        SimulationCheck::Rejected
    );
}
#[test]
fn incomplete_work_and_structural_errors_never_report_compiled() {
    let mut m = model("1");
    m.root.blocks[1]
        .parameters
        .insert("Opaque".into(), "x".repeat(2 * 1024 * 1024));
    let report = diagnose(&m, &compile_context());
    assert!(report.truncated);
    assert_eq!(report.simulation, SimulationCheck::Incomplete);
    let mut m = model("1");
    m.root.lines[0].dst.as_mut().unwrap().port.index = 999;
    let report = diagnose(&m, &compile_context());
    assert_eq!(report.simulation, SimulationCheck::Incomplete);
    assert!(report
        .diagnostics
        .iter()
        .any(|d| matches!(d.target, DiagnosticTarget::Line { root: 0, .. })));
}
#[test]
fn explicit_root_bindings_are_required_and_checked() {
    let mut m = model("1");
    m.root.blocks[0].block_type = "Inport".into();
    let mut context = compile_context();
    assert_eq!(diagnose(&m, &context).simulation, SimulationCheck::Rejected);
    context.inputs.insert("1".into(), "2".into());
    assert_eq!(diagnose(&m, &context).simulation, SimulationCheck::Compiled);
}
#[test]
fn flattened_compiler_ids_map_to_id_paths_not_names() {
    let mut m = model("missing");
    let create = unlinked_model::edit::Edit::CreateSubsystem {
        system: vec![],
        ids: vec!["1".into(), "2".into()],
        id: "10".into(),
        name: "a/b".into(),
    };
    unlinked_model::edit::apply_batch(&mut m, &[create]).unwrap();
    let report = diagnose(&m, &compile_context());
    assert_eq!(report.simulation, SimulationCheck::Rejected);
    assert!(report.diagnostics.iter().any(|d|d.severity==Severity::Error
        && matches!(&d.target,DiagnosticTarget::Block{system,id,..} if system==&vec!["10".into()] && id.0=="2")),"{:?}",report.diagnostics);
}
#[test]
fn report_and_context_have_worker_safe_json_contracts() {
    let ctx = compile_context();
    let decoded: DiagnosticContext =
        serde_json::from_str(&serde_json::to_string(&ctx).unwrap()).unwrap();
    let report = diagnose(&model("1"), &decoded);
    let decoded: unlinked_sim::diagnose::DiagnosticReport =
        serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
    assert_eq!(decoded.simulation, SimulationCheck::Compiled);
}

#[test]
fn invalid_explicit_run_options_use_the_simulators_checks() {
    let mut context = compile_context();
    context.options.as_mut().unwrap().max_samples = 0;
    assert_eq!(
        diagnose(&model("1"), &context).simulation,
        SimulationCheck::Rejected
    );
    let mut context = compile_context();
    let options = context.options.as_mut().unwrap();
    options.solver = unlinked_sim::Solver::Rk45;
    options.relative_tolerance = 2.0;
    assert_eq!(
        diagnose(&model("1"), &context).simulation,
        SimulationCheck::Rejected
    );
}
