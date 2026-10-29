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
        // Library blocks are `Reference` blocks linked to their source.
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
        library_source: descriptor.source_block.map(str::to_owned),
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
        "DiscretePulseGenerator",
        "SignalGenerator",
        "Ramp",
        "ChirpSignal",
        "RepeatingSequence",
        "Saturate",
        "DeadZone",
        "Quantizer",
        "CoulombFriction",
        "WrapToZero",
        "SaturationDynamic",
        "DeadZoneDynamic",
        "TransferFcn",
        "StateSpace",
        "Switch",
        "ManualSwitch",
        "MultiPortSwitch",
        "Abs",
        "Trigonometry",
        "Math",
        "Sqrt",
        "Signum",
        "Rounding",
        "UnaryMinus",
        "Polyval",
        "DotProduct",
        "MinMax",
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
                "Sin" | "SignalGenerator" => t.sin(),
                "Clock" | "Ramp" => t,
                "Ground" => 0.0,
                // Period 10 s, 5 % duty: high for the first 0.5 s.
                "DiscretePulseGenerator" => {
                    if t < 0.5 {
                        1.0
                    } else {
                        0.0
                    }
                }
                // 0.1 Hz to 1 Hz over 100 s.
                "ChirpSignal" => (std::f64::consts::TAU * (0.1 * t + 0.9 / 200.0 * t * t)).sin(),
                // [0 2] -> [0 2] wraps at the 2 s stop time.
                "RepeatingSequence" => {
                    if t < 2.0 {
                        t
                    } else {
                        0.0
                    }
                }
                "Saturate" => 0.5,
                // Constant 2 through each default: 2 - 0.5 past the zone end;
                // 2 is already a multiple of 0.5; 1 * |2| + 1 with sign(2);
                // 2 <= 255 passes; the dynamic limits are the input itself.
                "DeadZone" => 1.5,
                "CoulombFriction" => 3.0,
                "DeadZoneDynamic" => 0.0,
                "TransferFcn" => 2.0 * (1.0 - (-t).exp()),
                "StateSpace" => 2.0 * t.exp(),
                "Trigonometry" => 2.0_f64.sin(),
                "Math" => 2.0_f64.exp(),
                "Sqrt" => 2.0_f64.sqrt(),
                "Signum" => 1.0,
                "UnaryMinus" => -2.0,
                "DotProduct" => 4.0,
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
#[test]
fn library_palette_blocks_are_linked_references_that_simulate() {
    use unlinked_model::edit::{next_sid, Edit};
    let wire = |src: &str, dst: &str, index: u32| Line {
        src: Some(Endpoint {
            block: src.into(),
            port: PortRef {
                kind: PortKind::Out,
                index: 1,
            },
        }),
        dst: Some(Endpoint {
            block: dst.into(),
            port: PortRef {
                kind: PortKind::In,
                index,
            },
        }),
        ..Default::default()
    };
    for (key, first, second, expected) in [
        ("IndexVector", "1", "[5 6]", 6.0),
        ("EnvironmentController", "4", "9", 4.0),
    ] {
        let mut m = model("Constant");
        m.root.blocks.truncate(1);
        m.root.blocks[0]
            .parameters
            .insert("Value".into(), first.into());
        let mut other = block("other", "Constant");
        other.parameters.insert("Value".into(), second.into());
        m.root.blocks.push(other);
        let sid = next_sid(&m).unwrap().to_string();
        Edit::AddBlock {
            system: vec![],
            id: sid.as_str().into(),
            block_type: key.into(),
            name: "linked".into(),
            position: Rect::new(100.0, 0.0, 130.0, 30.0),
        }
        .apply(&mut m)
        .unwrap_or_else(|e| panic!("{key}: {e}"));
        let added = m.root.blocks.last().unwrap().clone();
        let descriptor = catalog::find(key).unwrap();
        assert_eq!(added.block_type, descriptor.mdl_block_type);
        assert_eq!(added.library_source.as_deref(), descriptor.source_block);
        if key == "IndexVector" {
            // Simulink saves the unmasked built-in library block as a plain
            // one-port, zero-based Multiport Switch; so does the palette.
            assert_eq!(added.block_type, "MultiPortSwitch");
            assert_eq!(added.parameters["Inputs"], "1");
            assert_eq!(added.parameters["DataPortOrder"], "Zero-based contiguous");
            assert_eq!(
                catalog::descriptor_for(&added).unwrap().type_key,
                "MultiPortSwitch"
            );
        } else {
            assert_eq!(added.block_type, "Reference");
            assert_eq!(catalog::descriptor_for(&added).unwrap().type_key, key);
        }
        assert_eq!(added.ports.inputs, 2);
        m.root.lines = vec![wire("source", &sid, 1), wire("other", &sid, 2)];
        assert!(unlinked_model::validation::validate_structure(&m).is_valid());
        let trace = simulate_model(&m, &options()).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert!(trace.signals[&sid].iter().all(|&v| v == expected), "{key}");
    }
}
