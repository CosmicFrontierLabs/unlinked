//! Palette defaults are executable configurations, not blanket promises about
//! every alternate parameter value exposed by the editor.
use unlinked_model::{catalog, *};
use unlinked_sim::{simulate_model, Options, Solver};

fn block(id: &str, key: &str) -> Block {
    let descriptor = catalog::find(key).unwrap();
    let parameters = descriptor.creation_parameters();
    let catalog::PortResolution::Known(ports) = descriptor.resolve_ports(&parameters) else {
        panic!("unresolved creation ports for {key}")
    };
    Block {
        id: id.into(),
        name: id.into(),
        block_type: descriptor.mdl_block_type.into(),
        position: Rect::new(
            0.0,
            0.0,
            descriptor.default_size[0],
            descriptor.default_size[1],
        ),
        orientation: Orientation::Right,
        mirrored: false,
        ports,
        parameters,
        mask: None,
        library_source: None,
        subsystem: None,
        style: Default::default(),
        interface: None,
    }
}
fn model(key: &str) -> Model {
    let subject = block("subject", key);
    let mut source = block("source", "Constant");
    source.parameters.insert("Value".into(), "2".into());
    let lines = (1..=subject.ports.inputs)
        .map(|index| Line {
            src: Some(Endpoint {
                block: source.id.clone(),
                port: PortRef {
                    kind: PortKind::Out,
                    index: 1,
                },
            }),
            dst: Some(Endpoint {
                block: subject.id.clone(),
                port: PortRef {
                    kind: PortKind::In,
                    index,
                },
            }),
            ..Default::default()
        })
        .collect();
    Model {
        name: "palette".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: Default::default(),
        workspace: Default::default(),
        type_defaults: Default::default(),
        charts: vec![],
        root: System {
            blocks: vec![source, subject],
            lines,
            ..Default::default()
        },
    }
}
fn options() -> Options {
    Options {
        stop: 2.0,
        step: 0.01,
        solver: Solver::Rk4,
        ..Default::default()
    }
}
#[test]
fn control_palette_defaults_match_analytic_responses() {
    for key in [
        "Step",
        "Sin",
        "Clock",
        "Ground",
        "Saturate",
        "TransferFcn",
        "StateSpace",
        "Switch",
        "Abs",
        "Trigonometry",
        "Terminator",
        "Display",
    ] {
        let model = model(key);
        assert!(
            unlinked_model::validation::validate_structure(&model).is_valid(),
            "{key}"
        );
        let trace = simulate_model(&model, &options()).unwrap_or_else(|e| panic!("{key}: {e}"));
        let values = &trace.signals["subject"];
        for (&t, &actual) in trace.time.iter().zip(values) {
            let expected = match key {
                "Step" => {
                    if t < 1.0 {
                        0.0
                    } else {
                        1.0
                    }
                }
                "Sin" => t.sin(),
                "Clock" => t,
                "Ground" => 0.0,
                "Saturate" => 0.5,
                "TransferFcn" => 2.0 * (1.0 - (-t).exp()),
                "StateSpace" => 2.0 * t.exp(),
                "Trigonometry" => 2.0_f64.sin(),
                _ => 2.0,
            };
            assert!(
                (actual - expected).abs() < 1e-7,
                "{key} at {t}: {actual} != {expected}"
            );
        }
    }
}
#[test]
fn random_default_is_repeatable_and_holds_between_sample_hits() {
    let model = model("RandomNumber");
    let trace = simulate_model(&model, &options()).unwrap();
    let repeated = simulate_model(&model, &options()).unwrap();
    assert_eq!(trace.signals, repeated.signals);
    let values = &trace.signals["subject"];
    assert!(values.iter().all(|v| v.is_finite()));
    assert!(values.windows(2).any(|v| v[0] != v[1]));
    for (i, pair) in values.windows(2).enumerate() {
        if (i + 1) % 10 != 0 {
            assert_eq!(pair[0], pair[1]);
        }
    }
}
#[test]
fn default_switch_selects_both_branches() {
    let mut model = model("Switch");
    let mut control = block("control", "Constant");
    control.parameters.insert("Value".into(), "0".into());
    let mut other = block("other", "Constant");
    other.parameters.insert("Value".into(), "7".into());
    model.root.blocks.extend([control, other]);
    model.root.lines[1].src.as_mut().unwrap().block = "control".into();
    model.root.lines[2].src.as_mut().unwrap().block = "other".into();
    assert!(
        simulate_model(&model, &options()).unwrap().signals["subject"]
            .iter()
            .all(|&v| v == 7.0)
    );
    model.root.blocks[2]
        .parameters
        .insert("Value".into(), "-1".into());
    assert!(
        simulate_model(&model, &options()).unwrap().signals["subject"]
            .iter()
            .all(|&v| v == 2.0)
    );
}
