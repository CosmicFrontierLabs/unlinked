use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::catalog::{
    find, validate_parameter, BlockDescriptor, ParameterDescriptor, ParameterKind, Visibility,
    BLOCKS,
};

fn parameter<'a>(block: &'a BlockDescriptor, name: &str) -> &'a ParameterDescriptor {
    block.parameters.iter().find(|p| p.name == name).unwrap()
}

#[test]
fn every_catalog_field_has_consistent_dialog_metadata() {
    assert_eq!(BLOCKS.len(), 25);
    for block in BLOCKS {
        for parameter in block.parameters {
            let dialog = &parameter.dialog;
            assert!(
                !dialog.section.trim().is_empty(),
                "{} {}",
                block.type_key,
                parameter.name
            );
            assert!(!dialog.help.trim().is_empty());
            assert_ne!(dialog.help, parameter.label);
            assert!(dialog.units.is_none_or(|u| !u.trim().is_empty()));
            let mut labels = BTreeSet::new();
            for label in dialog.enum_labels {
                let ParameterKind::Enum(values) = parameter.kind else {
                    panic!("friendly enum label attached to non-enum parameter");
                };
                assert!(values.contains(&label.value));
                assert!(labels.insert(label.value));
                assert!(!label.label.trim().is_empty());
            }
            match dialog.visible_when {
                Visibility::Always => {}
                Visibility::Equals {
                    parameter: controller,
                    value,
                }
                | Visibility::NotEquals {
                    parameter: controller,
                    value,
                } => {
                    assert_ne!(controller, parameter.name);
                    let controller = block
                        .parameters
                        .iter()
                        .find(|p| p.name == controller)
                        .unwrap();
                    assert!(matches!(
                        controller.kind,
                        ParameterKind::Enum(_) | ParameterKind::Boolean
                    ));
                    validate_parameter(controller, value).unwrap();
                }
            }
        }
    }
}

#[test]
fn visibility_uses_implicit_defaults_instead_of_creation_profiles() {
    let block = find("Switch").unwrap();
    let field = parameter(block, "Threshold");
    let imported = BTreeMap::new();
    // Imported Switch criteria default to >= Threshold; new blocks use ~= 0.
    assert!(field.visible(block, &imported));
    assert!(!field.visible(block, &block.creation_parameters()));
    for criterion in ["u2 >= Threshold", "u2 > Threshold"] {
        let params = BTreeMap::from([("Criteria".into(), criterion.into())]);
        assert!(field.visible(block, &params));
    }
    assert!(imported.is_empty());
}

#[test]
fn conditional_fields_follow_explicit_modes_without_changing_values() {
    let block = find("Integrator").unwrap();
    let initial = parameter(block, "InitialCondition");
    let saturation = parameter(block, "ShowSaturationPort");
    assert!(initial.visible(block, &BTreeMap::new()));
    assert!(!saturation.visible(block, &BTreeMap::new()));
    let params = BTreeMap::from([
        ("InitialConditionSource".into(), "external".into()),
        (
            "InitialCondition".into(),
            "my_preserved_initial_state".into(),
        ),
        ("LimitOutput".into(), "on".into()),
        ("ImportedExtra".into(), "preserve_me".into()),
    ]);
    let before = params.clone();
    assert!(!initial.visible(block, &params));
    assert!(saturation.visible(block, &params));
    assert_eq!(params, before);
    let scope = find("Scope").unwrap();
    assert!(!parameter(scope, "NumInputPorts")
        .visible(scope, &BTreeMap::from([("Floating".into(), "on".into())])));
    let sine = find("Sin").unwrap();
    for field in ["Frequency", "Phase"] {
        assert!(parameter(sine, field).visible(sine, &BTreeMap::new()));
        assert!(!parameter(sine, field).visible(
            sine,
            &BTreeMap::from([("SineType".into(), "Sample based".into())])
        ));
    }
}

#[test]
fn unknown_or_nonliteral_controller_values_remain_visible() {
    let block = find("Switch").unwrap();
    for raw in [
        "future_criteria",
        "evaluate_me()",
        "",
        "x".repeat(65 * 1024).as_str(),
    ] {
        let params = BTreeMap::from([("Criteria".into(), raw.into())]);
        assert!(parameter(block, "Threshold").visible(block, &params));
    }
    let block = find("Integrator").unwrap();
    let params = BTreeMap::from([("LimitOutput".into(), "some_variable".into())]);
    assert!(parameter(block, "ShowSaturationPort").visible(block, &params));
}

#[test]
fn friendly_labels_never_change_serialized_parameter_values() {
    let block = find("Gain").unwrap();
    let field = parameter(block, "Multiplication");
    let label = field
        .dialog
        .enum_labels
        .iter()
        .find(|l| l.value == "Matrix(K*u) (u vector)")
        .unwrap();
    assert_eq!(label.label, "Matrix: K * u (vector input)");
    validate_parameter(field, label.value).unwrap();
    assert!(validate_parameter(field, label.label).is_err());
    assert_eq!(
        block.creation_parameters()[field.name],
        "Element-wise(K.*u)"
    );
    let metadata = serde_json::to_value(field).unwrap();
    assert_eq!(metadata["dialog"]["section"], "Operation");
}

#[test]
fn grouped_dialog_orders_controllers_and_keeps_hidden_fields() {
    for block in BLOCKS {
        let groups = block.dialog_sections(&BTreeMap::new());
        let fields: Vec<_> = groups.iter().flat_map(|s| &s.fields).collect();
        assert_eq!(fields.len(), block.parameters.len());
        for (index, (field, visible)) in fields.iter().enumerate() {
            assert_eq!(*visible, field.visible(block, &BTreeMap::new()));
            let controller = match field.dialog.visible_when {
                Visibility::Always => continue,
                Visibility::Equals { parameter, .. } | Visibility::NotEquals { parameter, .. } => {
                    parameter
                }
            };
            let controlling_index = fields
                .iter()
                .position(|(p, _)| p.name == controller)
                .unwrap();
            assert!(
                controlling_index < index,
                "{} {}",
                block.type_key,
                field.name
            );
        }
    }
    let switch = find("Switch").unwrap();
    let groups = switch.dialog_sections(&switch.creation_parameters());
    assert!(groups
        .iter()
        .flat_map(|s| &s.fields)
        .any(|(p, visible)| p.name == "Threshold" && !visible));
    let field = parameter(find("Gain").unwrap(), "Multiplication");
    assert_eq!(field.label_for("Matrix(K*u)"), "Matrix: K * u");
    assert_eq!(field.label_for(" future mode "), " future mode ");
}

#[test]
fn mode_dependent_bounds_and_sample_settings_are_available() {
    let integrator = find("Integrator").unwrap();
    for (name, default, controller) in [
        ("UpperSaturationLimit", "inf", "LimitOutput"),
        ("LowerSaturationLimit", "-inf", "LimitOutput"),
        ("WrappedStateUpperValue", "pi", "WrapState"),
        ("WrappedStateLowerValue", "-pi", "WrapState"),
    ] {
        let p = parameter(integrator, name);
        assert_eq!(p.implicit_default, Some(default));
        assert!(!p.visible(integrator, &BTreeMap::new()));
        assert!(p.visible(
            integrator,
            &BTreeMap::from([(controller.into(), "on".into())])
        ));
    }
    let sine = find("Sin").unwrap();
    let sample = BTreeMap::from([
        ("SineType".into(), "Sample based".into()),
        ("TimeSource".into(), "Use external signal".into()),
    ]);
    assert!(!parameter(sine, "TimeSource").visible(sine, &sample));
    for (name, default) in [("Samples", "10"), ("Offset", "0")] {
        let p = parameter(sine, name);
        assert_eq!(p.implicit_default, Some(default));
        assert!(p.visible(sine, &sample));
        assert!(!p.visible(sine, &BTreeMap::new()));
    }
    let unlinked_model::catalog::PortResolution::Known(ports) = sine.resolve_ports(&sample) else {
        panic!("known sample arity")
    };
    assert_eq!(ports.inputs, 0);
    assert!(parameter(sine, "SineType").affects_ports);
}
