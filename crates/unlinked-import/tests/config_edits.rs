//! Solver settings edits are written where the importer reads them, change
//! nothing else in the file, and never pick a solver of their own.

use std::io::{Cursor, Read, Write};
use unlinked_import::patch::apply_edits;
use unlinked_model::edit::{apply_batch, Edit};

fn set(key: &str, value: &str) -> Edit {
    Edit::SetConfig {
        key: key.into(),
        value: value.into(),
    }
}

/// Apply `edits` to the file and to its IR, check both agree, and return
/// the new file.
fn roundtrip(name: &str, bytes: &[u8], edits: &[Edit]) -> Vec<u8> {
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    apply_batch(&mut expected, edits).unwrap();
    let out = apply_edits(name, bytes, edits).unwrap();
    let actual = unlinked_import::import(name, &out).unwrap();
    assert_eq!(actual.config, expected.config);
    out
}

const SOLVER_CC: &str = "Model {\n  Name \"m\"\n  Array {\n    Simulink.ConfigSet {\n      Array {\n\tSimulink.SolverCC {\n\t  StartTime\t\t  \"0.0\"\n\t  StopTime\t\t  \"10.0\"\n\t  Solver\t\t  \"ode45\"\n\t  SolverName\t\t  \"ode45\"\n\t}\n      }\n    }\n  }\n  System {\n    Name \"m\"\n  }\n}\n";

#[test]
fn mdl_solver_component_is_edited_in_place() {
    let out = roundtrip(
        "m.mdl",
        SOLVER_CC.as_bytes(),
        &[
            set("StopTime", "20"),
            set("Solver", "ode4"),
            set("FixedStep", "0.01"),
        ],
    );
    let text = String::from_utf8(out).unwrap();
    // Edited properties are rewritten in place and the new one joins the
    // component; nothing else changes.
    let expected = SOLVER_CC
        .replace("StopTime\t\t  \"10.0\"", "StopTime\t\"20\"")
        .replace("Solver\t\t  \"ode45\"", "Solver\t\"ode4\"")
        .replace(
            "SolverName\t\t  \"ode45\"\n",
            "SolverName\t\"ode4\"\n\t  FixedStep\t\"0.01\"\n",
        );
    assert_eq!(text, expected);
    let model = unlinked_import::import("m.mdl", text.as_bytes()).unwrap();
    assert_eq!(model.config.solver.as_deref(), Some("ode4"));
    assert_eq!(model.config.raw["SolverName"], "ode4");
    assert_eq!(model.config.fixed_step.as_deref(), Some("0.01"));
}

#[test]
fn mdl_without_a_configuration_set_writes_model_properties() {
    let src = "Model {\n  Name \"m\"\n  Solver \"ode4\"\n  StopTime \"1\"\n  System {\n    Name \"m\"\n  }\n}\n";
    let out = roundtrip(
        "m.mdl",
        src.as_bytes(),
        &[set("StopTime", "5"), set("RelTol", "1e-6")],
    );
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("StopTime \"5\"") || text.contains("StopTime\t\"5\""));
    assert!(text.contains("RelTol\t\"1e-6\"\n  System {"), "{text}");
}

#[test]
fn unknown_solvers_are_kept_verbatim() {
    let out = roundtrip("m.mdl", SOLVER_CC.as_bytes(), &[set("Solver", "ode15s")]);
    let model = unlinked_import::import("m.mdl", &out).unwrap();
    assert_eq!(model.config.solver.as_deref(), Some("ode15s"));
}

#[test]
fn only_solver_settings_are_editable() {
    let bytes = SOLVER_CC.as_bytes();
    for edit in [set("SignalLogging", "on"), set("StopTime", "1\n}")] {
        let mut model = unlinked_import::import("m.mdl", bytes).unwrap();
        assert!(apply_batch(&mut model, std::slice::from_ref(&edit)).is_err());
        assert!(apply_edits("m.mdl", bytes, &[edit]).is_err());
    }
}

fn slx(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in parts {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

const DIAGRAM: &str =
    r#"<ModelInformation><Model Name="m"><System></System></Model></ModelInformation>"#;
const CONFIG: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ConfigSet>
  <Object Version="1.18.0" ClassName="Simulink.ConfigSet">
    <Array PropName="Components" Type="Handle" Dimension="1*2">
      <Object ObjectID="2" Version="1.18.0" ClassName="Simulink.SolverCC">
        <P Name="StartTime">0.0</P>
        <P Name="StopTime">15</P>
        <P Name="Solver">VariableStepAuto</P>
        <P Name="SolverName">VariableStepAuto</P>
      </Object>
      <Object ObjectID="3" Version="1.18.0" ClassName="Simulink.DataIOCC">
        <P Name="Decimation">1</P>
      </Object>
    </Array>
  </Object>
</ConfigSet>
"#;

#[test]
fn slx_config_part_is_edited_and_other_parts_are_untouched() {
    let bytes = slx(&[
        ("simulink/blockdiagram.xml", DIAGRAM),
        ("simulink/configSet0.xml", CONFIG),
    ]);
    let out = roundtrip(
        "m.slx",
        &bytes,
        &[set("Solver", "ode4"), set("FixedStep", "1e-3")],
    );
    assert_eq!(part(&out, "simulink/blockdiagram.xml"), DIAGRAM);
    let expected = CONFIG.replace(">VariableStepAuto<", ">ode4<").replace(
        "        <P Name=\"SolverName\">ode4</P>\n",
        "        <P Name=\"SolverName\">ode4</P>\n        <P Name=\"FixedStep\">1e-3</P>\n",
    );
    assert_eq!(part(&out, "simulink/configSet0.xml"), expected);
}

#[test]
fn slx_without_a_config_part_writes_model_properties() {
    let bytes = slx(&[("simulink/blockdiagram.xml", DIAGRAM)]);
    let out = roundtrip("m.slx", &bytes, &[set("StopTime", "3")]);
    assert!(
        part(&out, "simulink/blockdiagram.xml").contains(r#"<P Name="StopTime">3</P>"#),
        "{}",
        part(&out, "simulink/blockdiagram.xml")
    );
}

/// Mirrored solver names split between the component and model-level
/// fallbacks are each written where they are read.
#[test]
fn mirrors_split_across_component_and_model_level_stay_in_step() {
    for src in [
        "Model {\nName m\nSolverName ode45\nSimulink.SolverCC {\nSolver ode4\n}\nSystem {\nName m\n}\n}\n",
        "Model {\nName m\nSolver ode45\nSimulink.SolverCC {\nSolverName ode4\n}\nSystem {\nName m\n}\n}\n",
    ] {
        let out = roundtrip("m.mdl", src.as_bytes(), &[set("Solver", "ode1")]);
        let model = unlinked_import::import("m.mdl", &out).unwrap();
        assert_eq!(model.config.raw["Solver"], "ode1");
        assert_eq!(model.config.raw["SolverName"], "ode1");
    }
}

#[test]
fn mdl_solver_objects_named_by_class_are_components() {
    let src = "Model {\nName m\nObject {\nClassName Simulink.SolverCC\nSolver ode4\n}\nSystem {\nName m\n}\n}\n";
    let out = roundtrip("m.mdl", src.as_bytes(), &[set("Solver", "ode1")]);
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("ClassName Simulink.SolverCC\nSolver\tode1\n"),
        "{text}"
    );
}

/// Ambiguous layouts are refused rather than guessed at.
#[test]
fn ambiguous_solver_settings_are_refused() {
    let edit = [set("Solver", "ode1")];
    for src in [
        "Model {\nName m\nSimulink.SolverCC {\nSolver ode4\nSolver ode45\n}\nSystem {\nName m\n}\n}\n",
        "Model {\nName m\nSimulink.SolverCC {\nSolver ode4\n}\nSimulink.SolverCC {\nSolver ode45\n}\nSystem {\nName m\n}\n}\n",
    ] {
        assert!(apply_edits("m.mdl", src.as_bytes(), &edit).is_err(), "{src}");
    }
    let duplicate = slx(&[(
        "simulink/blockdiagram.xml",
        r#"<ModelInformation><Model Name="m"><Object ClassName="Simulink.SolverCC"><P Name="Solver">ode4</P><P Name="Solver">ode45</P></Object><System/></Model></ModelInformation>"#,
    )]);
    assert!(apply_edits("m.slx", &duplicate, &edit).is_err());
}

fn info(active: &[(&str, bool)]) -> String {
    let sets: String = active
        .iter()
        .map(|(part, on)| {
            let flag = if *on { r#" Active="true""# } else { "" };
            format!(r#"<ConfigSet PartName="/simulink/{part}"{flag}>Set</ConfigSet>"#)
        })
        .collect();
    format!("<ConfigSetInfo>{sets}</ConfigSetInfo>")
}

const SOLVER_ODE4: &str = r#"<ConfigSet><Object ClassName="Simulink.SolverCC"><P Name="Solver">ode4</P></Object></ConfigSet>"#;
const SOLVER_ODE45: &str = r#"<ConfigSet><Object ClassName="Simulink.SolverCC"><P Name="Solver">ode45</P></Object></ConfigSet>"#;

/// The active configuration set is the one read and written; the others
/// are left alone.
#[test]
fn the_active_configuration_set_is_read_and_edited() {
    let index = info(&[("configSet0.xml", false), ("configSet1.xml", true)]);
    let bytes = slx(&[
        ("simulink/blockdiagram.xml", DIAGRAM),
        ("simulink/configSetInfo.xml", &index),
        ("simulink/configSet0.xml", SOLVER_ODE4),
        ("simulink/configSet1.xml", SOLVER_ODE45),
    ]);
    let model = unlinked_import::import("m.slx", &bytes).unwrap();
    assert_eq!(model.config.solver.as_deref(), Some("ode45"));
    let out = roundtrip("m.slx", &bytes, &[set("Solver", "ode23")]);
    assert_eq!(part(&out, "simulink/configSet0.xml"), SOLVER_ODE4);
    assert!(part(&out, "simulink/configSet1.xml").contains(">ode23<"));
}

#[test]
fn unresolved_or_referenced_configuration_sets_are_refused() {
    let edit = [set("Solver", "ode1")];
    let reference = r#"<ConfigSet><Object ClassName="Simulink.ConfigSetRef"><P Name="SourceName">Shared</P></Object></ConfigSet>"#;
    let none_active = info(&[("configSet0.xml", false), ("configSet1.xml", false)]);
    let reference_active = info(&[("configSet0.xml", false), ("configSet1.xml", true)]);
    for (index, second) in [(&none_active, SOLVER_ODE45), (&reference_active, reference)] {
        let bytes = slx(&[
            ("simulink/blockdiagram.xml", DIAGRAM),
            ("simulink/configSetInfo.xml", index),
            ("simulink/configSet0.xml", SOLVER_ODE4),
            ("simulink/configSet1.xml", second),
        ]);
        // Neither imports the inactive set's solver as the model's.
        let model = unlinked_import::import("m.slx", &bytes).unwrap();
        assert_eq!(model.config.solver, None);
        assert!(apply_edits("m.slx", &bytes, &edit).is_err());
    }
}

#[test]
fn settings_held_only_in_typed_fields_survive_other_edits() {
    let mut model = unlinked_import::import("m.mdl", SOLVER_CC.as_bytes()).unwrap();
    model.config.fixed_step = Some("0.5".into());
    apply_batch(&mut model, &[set("StopTime", "3")]).unwrap();
    assert_eq!(model.config.fixed_step.as_deref(), Some("0.5"));
    assert_eq!(model.config.stop_time.as_deref(), Some("3"));
}

#[test]
fn configuration_sets_without_a_single_active_one_are_refused() {
    let edit = [set("Solver", "ode1")];
    let sets = [
        ("simulink/blockdiagram.xml", DIAGRAM),
        ("simulink/configSet0.xml", SOLVER_ODE4),
        ("simulink/configSet1.xml", SOLVER_ODE45),
    ];
    // Several sets and no index.
    assert!(apply_edits("m.slx", &slx(&sets), &edit).is_err());
    // Two active entries, one of them malformed.
    let index = r#"<ConfigSetInfo><ConfigSet Active="true" PartName="/simulink/configSet0.xml"/><ConfigSet Active="true"/></ConfigSetInfo>"#;
    let mut parts = sets.to_vec();
    parts.push(("simulink/configSetInfo.xml", index));
    assert!(apply_edits("m.slx", &slx(&parts), &edit).is_err());
    // An MDL configuration reference.
    let src = "Model {\nName m\nObject {\nClassName Simulink.ConfigSetRef\nSourceName shared\n}\nSystem {\nName m\n}\n}\n";
    assert!(apply_edits("m.mdl", src.as_bytes(), &edit).is_err());
}

/// A stored solver type follows the kind of a recognized solver, and is
/// left alone for one it does not recognize.
#[test]
fn solver_type_follows_the_solver() {
    let src = "Model {\n  Name \"m\"\n  Simulink.SolverCC {\n    Solver \"ode45\"\n    SolverType \"Variable-step\"\n  }\n  System {\n    Name \"m\"\n  }\n}\n";
    let out = roundtrip("m.mdl", src.as_bytes(), &[set("Solver", "ode4")]);
    let model = unlinked_import::import("m.mdl", &out).unwrap();
    assert_eq!(model.config.raw["SolverType"], "Fixed-step");
    assert!(unlinked_model::config::validate_config(&model.config).is_valid());
    let out = roundtrip("m.mdl", src.as_bytes(), &[set("Solver", "odeFuture")]);
    let model = unlinked_import::import("m.mdl", &out).unwrap();
    assert_eq!(model.config.raw["SolverType"], "Variable-step");
}
