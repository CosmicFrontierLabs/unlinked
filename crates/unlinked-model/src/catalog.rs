//! Creation metadata for a deliberately small native-block palette.
//!
//! Creation defaults are an Unlinked profile; implicit defaults are separately
//! recorded for native-block port inference and never written into imported files.
//! Port rules follow the MathWorks [Integrator](https://www.mathworks.com/help/simulink/slref/integrator.html),
//! [Mux](https://www.mathworks.com/help/simulink/slref/mux.html), and
//! [Demux](https://www.mathworks.com/help/simulink/slref/demux.html) references.
//! New Sum blocks use rectangular icons; ZOH deliberately starts with an explicit
//! one-second sample period. UnitDelay retains inherited sampling; multirate
//! simulation may require the user to choose an explicit period. New Switch
//! blocks use nonzero-control selection with zero-crossing detection disabled,
//! which is supported by the simulator; threshold modes remain editable.
//! Sine/trigonometry arity follows the MathWorks
//! [Sine Wave](https://www.mathworks.com/help/simulink/slref/sinewave.html) and
//! [Trigonometric Function](https://www.mathworks.com/help/simulink/slref/trigonometricfunction.html)
//! references. Creation-profile simulation tests live in unlinked-sim/tests/catalog.rs;
//! these do not imply support for every legal setting of a block.
//! This module never evaluates MATLAB, runs callbacks, or certifies simulation
//! support. Unknown imported types and parameters must remain preservable.
use crate::{Block, PortCounts, System};
use serde::Serialize;
use std::collections::BTreeMap;

pub const MAX_PORTS: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ParameterKind {
    Expression,
    IntegerExpression,
    Enum(&'static [&'static str]),
    Boolean,
    Text,
}

/// Labels are presentation only; `value` remains the serialized Simulink token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EnumLabel {
    pub value: &'static str,
    pub label: &'static str,
}

/// A bounded, nonrecursive predicate over one catalog enum/boolean parameter.
/// Expressions and callbacks are never evaluated to decide dialog visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Visibility {
    Always,
    Equals {
        parameter: &'static str,
        value: &'static str,
    },
    NotEquals {
        parameter: &'static str,
        value: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ParameterDialog {
    pub section: &'static str,
    pub help: &'static str,
    pub units: Option<&'static str>,
    pub enum_labels: &'static [EnumLabel],
    pub visible_when: Visibility,
}

impl ParameterDialog {
    pub const fn when(mut self, condition: Visibility) -> Self {
        self.visible_when = condition;
        self
    }
    pub const fn with_enum_labels(mut self, labels: &'static [EnumLabel]) -> Self {
        self.enum_labels = labels;
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParameterDescriptor {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParameterKind,
    /// Explicit value for newly created blocks (may differ from imported defaults).
    pub default: &'static str,
    /// Verified native-block implicit value when no effective imported value exists.
    pub implicit_default: Option<&'static str>,
    pub affects_ports: bool,
    pub dialog: ParameterDialog,
}

impl ParameterDescriptor {
    /// Friendly enum label, or the exact stored token when no label is known.
    pub fn label_for<'a>(&self, value: &'a str) -> &'a str {
        self.dialog
            .enum_labels
            .iter()
            .find(|label| label.value == value)
            .map_or(value, |label| label.label)
    }

    /// Whether to show this typed field. Explicit effective imported values win;
    /// absent values use the controlling parameter's implicit default, never its
    /// creation profile. Unknown, invalid or nonliteral controllers remain visible.
    /// Hidden values are not removed: callers must preserve the raw parameter map.
    pub fn visible(&self, block: &BlockDescriptor, parameters: &BTreeMap<String, String>) -> bool {
        let (name, wanted, equal) = match self.dialog.visible_when {
            Visibility::Always => return true,
            Visibility::Equals { parameter, value } => (parameter, value, true),
            Visibility::NotEquals { parameter, value } => (parameter, value, false),
        };
        let Some(controller) = block.parameters.iter().find(|p| p.name == name) else {
            return true;
        };
        let Some(actual) = parameters
            .get(name)
            .map(String::as_str)
            .or(controller.implicit_default)
        else {
            return true;
        };
        if actual.len() > 64 * 1024
            || !matches!(
                controller.kind,
                ParameterKind::Enum(_) | ParameterKind::Boolean
            )
            || validate_parameter(controller, actual).is_err()
        {
            return true;
        }
        (actual.trim() == wanted) == equal
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum PortRule {
    SineWave,
    Trigonometry,
    Display,
    Integrator,
    Scope,
    Fixed {
        inputs: u32,
        outputs: u32,
    },
    /// Sum/Product sign lists, or a positive integer expression.
    Signs {
        parameter: &'static str,
        signs: &'static str,
    },
    /// Scalar port count or a literal vector of positive widths (Mux/Demux).
    Count {
        parameter: &'static str,
        input: bool,
        widths: bool,
        other: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortResolution {
    Known(PortCounts),
    /// A missing value or MATLAB expression requires semantic resolution.
    Unresolved {
        parameter: &'static str,
    },
    Invalid {
        parameter: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BlockDescriptor {
    pub type_key: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub slx_block_type: &'static str,
    pub mdl_block_type: &'static str,
    pub source_block: Option<&'static str>,
    pub parameters: &'static [ParameterDescriptor],
    pub ports: PortRule,
    pub default_size: [f64; 2],
    pub creatable: bool,
}

/// Sections and fields retain catalog order; hidden fields remain represented
/// so callers can preserve their raw values instead of deleting them.
#[derive(Debug)]
pub struct DialogSection {
    pub name: &'static str,
    pub fields: Vec<(&'static ParameterDescriptor, bool)>,
}

impl BlockDescriptor {
    pub fn dialog_sections(&self, parameters: &BTreeMap<String, String>) -> Vec<DialogSection> {
        let mut sections: Vec<DialogSection> = Vec::new();
        for field in self.parameters {
            let index = sections
                .iter()
                .position(|section| section.name == field.dialog.section)
                .unwrap_or_else(|| {
                    sections.push(DialogSection {
                        name: field.dialog.section,
                        fields: Vec::new(),
                    });
                    sections.len() - 1
                });
            sections[index]
                .fields
                .push((field, field.visible(self, parameters)));
        }
        sections
    }

    /// Use only when creating a new block; never merge into imported params.
    pub fn creation_parameters(&self) -> BTreeMap<String, String> {
        self.parameters
            .iter()
            .map(|p| (p.name.into(), p.default.into()))
            .collect()
    }
    pub fn resolve_ports(&self, parameters: &BTreeMap<String, String>) -> PortResolution {
        for descriptor in self.parameters.iter().filter(|p| p.affects_ports) {
            if let Some(value) = parameters.get(descriptor.name) {
                if let Err(message) = validate_parameter(descriptor, value) {
                    return PortResolution::Invalid {
                        parameter: descriptor.name,
                        message,
                    };
                }
            }
        }
        let effective: BTreeMap<_, _> = self
            .parameters
            .iter()
            .filter(|p| p.affects_ports)
            .filter_map(|p| {
                parameters
                    .get(p.name)
                    .map(String::as_str)
                    .or(p.implicit_default)
                    .map(|value| (p.name.to_string(), value.to_string()))
            })
            .collect();
        self.ports.resolve(&effective)
    }
    /// Allocate an Inport/Outport number without changing existing blocks.
    /// Plain ports default to one. Bus elements reserve their shared interface
    /// number; malformed or conflicting numbering prevents allocation.
    pub fn creation_parameters_in(
        &self,
        system: &System,
    ) -> Result<BTreeMap<String, String>, String> {
        let mut parameters = self.creation_parameters();
        if matches!(self.type_key, "Inport" | "Outport") {
            let mut used = BTreeMap::new();
            for block in system
                .blocks
                .iter()
                .filter(|b| b.block_type == self.type_key)
            {
                let number = interface_port_number(block)?;
                if let Some(previous) = used.insert(number, block) {
                    if !share_interface(previous, block) {
                        return Err("conflicting interface port number".into());
                    }
                }
            }
            let number = (1..=MAX_PORTS)
                .find(|n| !used.contains_key(n))
                .ok_or("no free port number")?;
            parameters.insert("Port".into(), number.to_string());
        }
        Ok(parameters)
    }

    /// Validate an edit without mutating the block. The caller must reject any
    /// connection removal before committing the returned Known port counts.
    /// Unresolved counts require semantic resolution before rewiring.
    pub fn check_edit(
        &self,
        current: &BTreeMap<String, String>,
        name: &str,
        value: &str,
    ) -> Result<ParameterEdit, String> {
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || value.len() > 64 * 1024
        {
            return Err("invalid parameter name or value exceeds 64 KiB".into());
        }
        let descriptor = self.parameters.iter().find(|p| p.name == name);
        if let Some(descriptor) = descriptor {
            validate_parameter(descriptor, value)?;
        }
        if name == "Port" && matches!(self.type_key, "Inport" | "Outport") {
            port_number(value)?;
        }
        let previous_ports = self.resolve_ports(current);
        if !descriptor.is_some_and(|p| p.affects_ports) {
            return Ok(ParameterEdit {
                ports: previous_ports.clone(),
                previous_ports,
                changes_ports: Some(false),
            });
        }
        let mut edited = BTreeMap::new();
        for p in self.parameters.iter().filter(|p| p.affects_ports) {
            if let Some(v) = current.get(p.name) {
                edited.insert(p.name.to_string(), v.clone());
            }
        }
        edited.insert(name.into(), value.into());
        let ports = self.resolve_ports(&edited);
        if let PortResolution::Invalid { parameter, message } = &ports {
            return Err(format!("{parameter}: {message}"));
        }
        let changes_ports = match (&previous_ports, &ports) {
            (PortResolution::Known(before), PortResolution::Known(after)) => Some(before != after),
            _ => None,
        };
        Ok(ParameterEdit {
            previous_ports,
            ports,
            changes_ports,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterEdit {
    pub previous_ports: PortResolution,
    pub ports: PortResolution,
    /// None when a port-affecting edit cannot compare fully resolved counts.
    pub changes_ports: Option<bool>,
}

pub fn port_number(value: &str) -> Result<u32, String> {
    let number = value
        .trim()
        .parse::<u32>()
        .map_err(|_| "port number must be a positive integer literal")?;
    if !(1..=MAX_PORTS).contains(&number) {
        return Err(format!("port number must be between 1 and {MAX_PORTS}"));
    }
    Ok(number)
}

/// Effective interface number without conflating plain and bus-element ports.
pub fn interface_port_number(block: &Block) -> Result<u32, String> {
    if let Some(interface) = &block.interface {
        let number = interface
            .port_number
            .filter(|n| (1..=MAX_PORTS).contains(n))
            .ok_or("invalid or missing interface PortNumber")?;
        if interface
            .port_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
        {
            return Err("missing interface PortName".into());
        }
        if let Some(explicit) = block.param("Port") {
            if port_number(explicit)? != number {
                return Err("Port disagrees with interface PortNumber".into());
            }
        }
        Ok(number)
    } else {
        port_number(block.param("Port").unwrap_or("1"))
    }
}

/// Different bus elements may share a number only within the same named port.
pub fn share_interface(a: &Block, b: &Block) -> bool {
    match (&a.interface, &b.interface) {
        (Some(a), Some(b)) => a
            .port_name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty() && b.port_name.as_deref() == Some(name)),
        _ => false,
    }
}

impl PortRule {
    fn resolve(self, parameters: &BTreeMap<String, String>) -> PortResolution {
        let value = |key: &str| parameters.get(key).map(String::as_str).map(str::trim);
        let (parameter, input, widths, signs, other) = match self {
            Self::SineWave => {
                return known(
                    u32::from(
                        value("SineType") == Some("Time based")
                            && value("TimeSource") == Some("Use external signal"),
                    ),
                    1,
                );
            }
            Self::Trigonometry => {
                return known(
                    if value("Operator") == Some("atan2") {
                        2
                    } else {
                        1
                    },
                    if value("Operator") == Some("sincos") {
                        2
                    } else {
                        1
                    },
                );
            }
            Self::Display => return known(u32::from(value("Floating") != Some("on")), 0),
            Self::Integrator => {
                for parameter in [
                    "ExternalReset",
                    "InitialConditionSource",
                    "LimitOutput",
                    "ShowSaturationPort",
                    "ShowStatePort",
                ] {
                    if value(parameter).is_none() {
                        return PortResolution::Unresolved { parameter };
                    }
                }
                return PortResolution::Known(PortCounts {
                    inputs: 1
                        + u32::from(value("ExternalReset") != Some("none"))
                        + u32::from(value("InitialConditionSource") == Some("external")),
                    outputs: 1 + u32::from(
                        value("LimitOutput") == Some("on")
                            && value("ShowSaturationPort") == Some("on"),
                    ),
                    state: u32::from(value("ShowStatePort") == Some("on")),
                    ..PortCounts::default()
                });
            }
            Self::Scope => {
                if value("Floating") == Some("on") {
                    return known(0, 0);
                }
                return Self::Count {
                    parameter: "NumInputPorts",
                    input: true,
                    widths: false,
                    other: 0,
                }
                .resolve(parameters);
            }
            Self::Fixed { inputs, outputs } => return known(inputs, outputs),
            Self::Signs { parameter, signs } => (parameter, true, false, Some(signs), 1),
            Self::Count {
                parameter,
                input,
                widths,
                other,
            } => (parameter, input, widths, None, other),
        };
        let unresolved = || PortResolution::Unresolved { parameter };
        let invalid = || PortResolution::Invalid {
            parameter,
            message: format!(
                "expected a port count from 1 to {MAX_PORTS}, or a supported port specification"
            ),
        };
        let Some(raw) = parameters.get(parameter) else {
            return unresolved();
        };
        if raw.len() > 64 * 1024 {
            return invalid();
        }
        let raw = raw.trim();
        if raw.is_empty() {
            return invalid();
        }
        let count = if let Some(signs) = signs.filter(|_| {
            raw.chars()
                .any(|c| matches!(c, '+' | '-' | '*' | '/' | '|'))
        }) {
            if raw
                .chars()
                .all(|c| signs.contains(c) || (signs == "+-" && c == '|') || c.is_whitespace())
            {
                Some(raw.chars().filter(|&c| signs.contains(c)).count() as f64)
            } else {
                None
            }
        } else {
            None
        };
        if count.is_none()
            && raw
                .chars()
                .all(|c| c.is_whitespace() || "+-*/|".contains(c))
        {
            return invalid();
        }
        let count = count.or_else(|| raw.parse::<f64>().ok());
        let count = if let Some(count) = count {
            count
        } else if widths && raw.starts_with('[') && raw.ends_with(']') {
            // Literal row/column vectors only. MATLAB expressions, ranges,
            // cell arrays and named Mux ports require semantic resolution.
            let inner = &raw[1..raw.len() - 1];
            let mut n = 0;
            let mut only_width = 0.0;
            let mut row_lengths = Vec::new();
            for row in inner.split(';') {
                let mut row_len = 0;
                for token in row
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|s| !s.is_empty())
                {
                    let Ok(width) = token.parse::<f64>() else {
                        return unresolved();
                    };
                    if !width.is_finite() || (width != -1.0 && width < 1.0) || width.fract() != 0.0
                    {
                        return invalid();
                    }
                    only_width = width;
                    n += 1;
                    row_len += 1;
                    if n > MAX_PORTS {
                        return invalid();
                    }
                }
                if row_len > 0 {
                    row_lengths.push(row_len);
                }
            }
            if row_lengths.len() > 1 && row_lengths.iter().any(|&n| n != 1) {
                return invalid();
            }
            if n == 1 {
                only_width
            } else {
                n as f64
            }
        } else {
            return unresolved();
        };
        if !count.is_finite() || count < 1.0 || count > MAX_PORTS as f64 || count.fract() != 0.0 {
            return invalid();
        }
        if input {
            known(count as u32, other)
        } else {
            known(other, count as u32)
        }
    }
}

fn known(inputs: u32, outputs: u32) -> PortResolution {
    PortResolution::Known(PortCounts {
        inputs,
        outputs,
        ..PortCounts::default()
    })
}

/// Schema checking intentionally leaves expressions unevaluated. This checks
/// a single explicit value, without supplying defaults or rejecting unknown keys.
pub fn validate_parameter(parameter: &ParameterDescriptor, value: &str) -> Result<(), String> {
    if value.len() > 64 * 1024 {
        return Err("parameter exceeds 64 KiB".into());
    }
    let value = value.trim();
    match parameter.kind {
        ParameterKind::Enum(choices) if !choices.contains(&value) => {
            Err(format!("expected one of {}", choices.join(", ")))
        }
        ParameterKind::Boolean if !matches!(value, "on" | "off") => {
            Err("expected on or off".into())
        }
        ParameterKind::Expression | ParameterKind::IntegerExpression if value.is_empty() => {
            Err("expression is empty".into())
        }
        ParameterKind::IntegerExpression => {
            if let Ok(n) = value.parse::<f64>() {
                if !n.is_finite() || n.fract() != 0.0 || n < 1.0 || n > MAX_PORTS as f64 {
                    return Err(
                        "expected a positive integer expression within the port limit".into(),
                    );
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

const fn param(
    name: &'static str,
    label: &'static str,
    kind: ParameterKind,
    default: &'static str,
    affects_ports: bool,
    dialog: ParameterDialog,
) -> ParameterDescriptor {
    ParameterDescriptor {
        name,
        label,
        kind,
        default,
        implicit_default: Some(default),
        affects_ports,
        dialog,
    }
}
const fn dialog(
    section: &'static str,
    help: &'static str,
    units: Option<&'static str>,
) -> ParameterDialog {
    ParameterDialog {
        section,
        help,
        units,
        enum_labels: &[],
        visible_when: Visibility::Always,
    }
}
const fn block(
    type_key: &'static str,
    label: &'static str,
    category: &'static str,
    parameters: &'static [ParameterDescriptor],
    ports: PortRule,
    default_size: [f64; 2],
) -> BlockDescriptor {
    BlockDescriptor {
        type_key,
        label,
        category,
        slx_block_type: type_key,
        mdl_block_type: type_key,
        source_block: None,
        parameters,
        ports,
        default_size,
        creatable: true,
    }
}
use ParameterKind::{Boolean, Enum, Expression as Expr, IntegerExpression as Int};
const ONE: PortRule = PortRule::Fixed {
    inputs: 1,
    outputs: 1,
};
const SOURCE: PortRule = PortRule::Fixed {
    inputs: 0,
    outputs: 1,
};
const SINK: PortRule = PortRule::Fixed {
    inputs: 1,
    outputs: 0,
};

pub static BLOCKS: &[BlockDescriptor] = &[
    block(
        "Constant",
        "Constant",
        "Sources",
        &[
            param(
                "Value",
                "Value",
                Expr,
                "1",
                false,
                dialog(
                    "Signal",
                    "MATLAB scalar or array expression emitted by the constant source.",
                    None,
                ),
            ),
            param(
                "SampleTime",
                "Sample time",
                Expr,
                "inf",
                false,
                dialog(
                    "Timing",
                    "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                    Some("s"),
                ),
            ),
        ],
        SOURCE,
        [30.0, 30.0],
    ),
    block(
        "Gain",
        "Gain",
        "Math",
        &[
            param(
                "Gain",
                "Gain",
                Expr,
                "1",
                false,
                dialog(
                    "Operation",
                    "Scalar, vector, or matrix gain expression K; the multiplication mode determines how K combines with the input.",
                    None,
                ),
            ),
            param(
                "Multiplication",
                "Multiplication",
                Enum(
                    &[
                        "Element-wise(K.*u)",
                        "Matrix(K*u)",
                        "Matrix(u*K)",
                        "Matrix(K*u) (u vector)",
                    ],
                ),
                "Element-wise(K.*u)",
                false,
                dialog(
                        "Operation",
                        "Choose elementwise multiplication, K*u, or u*K; dimensions must agree with the chosen operation.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "Element-wise(K.*u)",
                                label: "Elementwise: K .* u",
                            },
                            EnumLabel {
                                value: "Matrix(K*u)",
                                label: "Matrix: K * u",
                            },
                            EnumLabel {
                                value: "Matrix(u*K)",
                                label: "Matrix: u * K",
                            },
                            EnumLabel {
                                value: "Matrix(K*u) (u vector)",
                                label: "Matrix: K * u (vector input)",
                            },
                        ],
                    ),
            ),
        ],
        ONE,
        [30.0, 30.0],
    ),
    block(
        "Sum",
        "Sum",
        "Math",
        &[
            param(
                "Inputs",
                "Inputs",
                Expr,
                "++",
                true,
                dialog(
                    "Operation",
                    "A sign per input, such as ++ or +-, or an integer input count. Changes the number of input ports.",
                    None,
                ),
            ),
            param(
                "IconShape",
                "Icon shape",
                Enum(&["rectangular", "round"]),
                "rectangular",
                false,
                dialog(
                        "Appearance",
                        "Choose the Sum block icon shape; this does not change its arithmetic.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "rectangular",
                                label: "Rectangular",
                            },
                            EnumLabel {
                                value: "round",
                                label: "Round",
                            },
                        ],
                    ),
            ),
        ],
        PortRule::Signs {
            parameter: "Inputs",
            signs: "+-",
        },
        [30.0, 31.0],
    ),
    block(
        "Product",
        "Product",
        "Math",
        &[
            ParameterDescriptor {
                implicit_default: Some("2"),
                ..param(
                    "Inputs",
                    "Inputs",
                    Expr,
                    "**",
                    true,
                    dialog(
                        "Operation",
                        "A multiplication or division sign per input, such as ** or */, or an integer input count.",
                        None,
                    ),
                )
            },
            param(
                "Multiplication",
                "Multiplication",
                Enum(&["Element-wise(.*)", "Matrix(*)"]),
                "Element-wise(.*)",
                false,
                dialog(
                        "Operation",
                        "Choose elementwise or matrix multiplication and division.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "Element-wise(.*)",
                                label: "Elementwise",
                            },
                            EnumLabel {
                                value: "Matrix(*)",
                                label: "Matrix",
                            },
                        ],
                    ),
            ),
        ],
        PortRule::Signs {
            parameter: "Inputs",
            signs: "*/",
        },
        [30.0, 31.0],
    ),
    block(
        "Integrator",
        "Integrator",
        "Continuous",
        &[
            param(
                "InitialConditionSource",
                "Initial condition source",
                Enum(&["internal", "external"]),
                "internal",
                true,
                dialog(
                        "State",
                        "Use the initial-condition expression or obtain the initial condition from an additional input port.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "internal",
                                label: "Parameter",
                            },
                            EnumLabel {
                                value: "external",
                                label: "Input port",
                            },
                        ],
                    ),
            ),
            param(
                "InitialCondition",
                "Initial condition",
                Expr,
                "0",
                false,
                dialog(
                        "State",
                        "Value of the state before the first update; enter a MATLAB scalar or array expression.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "InitialConditionSource",
                        value: "internal",
                    }),
            ),
            param(
                "ExternalReset",
                "External reset",
                Enum(&["none", "rising", "falling", "either", "level", "level hold"]),
                "none",
                true,
                dialog(
                        "State",
                        "Select which external reset events restore the initial condition; enabled modes add a reset input.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "none",
                                label: "None",
                            },
                            EnumLabel {
                                value: "rising",
                                label: "Rising edge",
                            },
                            EnumLabel {
                                value: "falling",
                                label: "Falling edge",
                            },
                            EnumLabel {
                                value: "either",
                                label: "Either edge",
                            },
                            EnumLabel {
                                value: "level",
                                label: "Level",
                            },
                            EnumLabel {
                                value: "level hold",
                                label: "Level hold",
                            },
                        ],
                    ),
            ),
            param(
                "ShowStatePort",
                "Show state port",
                Boolean,
                "off",
                true,
                dialog(
                    "Ports",
                    "Expose the integrator state on a state port; changes the block interface.",
                    None,
                ),
            ),
            param(
                "LimitOutput",
                "Limit output",
                Boolean,
                "off",
                true,
                dialog(
                    "Limits",
                    "Enable output saturation. Set the upper and lower saturation limits below.",
                    None,
                ),
            ),
            param(
                "UpperSaturationLimit",
                "Upper saturation limit",
                Expr,
                "inf",
                false,
                dialog(
                        "Limits",
                        "Upper saturation limit as a MATLAB scalar or array expression.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "LimitOutput",
                        value: "on",
                    }),
            ),
            param(
                "LowerSaturationLimit",
                "Lower saturation limit",
                Expr,
                "-inf",
                false,
                dialog(
                        "Limits",
                        "Lower saturation limit as a MATLAB scalar or array expression.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "LimitOutput",
                        value: "on",
                    }),
            ),
            param(
                "ShowSaturationPort",
                "Show saturation port",
                Boolean,
                "off",
                true,
                dialog(
                        "Limits",
                        "Expose saturation status when output limiting is enabled; changes the output port count.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "LimitOutput",
                        value: "on",
                    }),
            ),
            param(
                "WrapState",
                "Wrap state",
                Boolean,
                "off",
                false,
                dialog(
                    "State",
                    "Wrap the integrator state at its configured bounds. Set the upper and lower wrapped state values below.",
                    None,
                ),
            ),
            param(
                "WrappedStateUpperValue",
                "Wrapped state upper value",
                Expr,
                "pi",
                false,
                dialog(
                        "State",
                        "Wrapped state upper value as a MATLAB scalar or array expression.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "WrapState",
                        value: "on",
                    }),
            ),
            param(
                "WrappedStateLowerValue",
                "Wrapped state lower value",
                Expr,
                "-pi",
                false,
                dialog(
                        "State",
                        "Wrapped state lower value as a MATLAB scalar or array expression.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "WrapState",
                        value: "on",
                    }),
            ),
        ],
        PortRule::Integrator,
        [40.0, 40.0],
    ),
    block(
        "UnitDelay",
        "Unit Delay",
        "Discrete",
        &[
            param(
                "InitialCondition",
                "Initial condition",
                Expr,
                "0",
                false,
                dialog(
                    "State",
                    "Value of the state before the first update; enter a MATLAB scalar or array expression.",
                    None,
                ),
            ),
            param(
                "SampleTime",
                "Sample time",
                Expr,
                "-1",
                false,
                dialog(
                    "Timing",
                    "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                    Some("s"),
                ),
            ),
        ],
        ONE,
        [35.0, 34.0],
    ),
    block(
        "ZeroOrderHold",
        "Zero-Order Hold",
        "Discrete",
        &[
            ParameterDescriptor {
                implicit_default: Some("-1"),
                ..param(
                    "SampleTime",
                    "Sample time",
                    Expr,
                    "1",
                    false,
                    dialog(
                        "Timing",
                        "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                        Some("s"),
                    ),
                )
            },
        ],
        ONE,
        [35.0, 30.0],
    ),
    block(
        "Scope",
        "Scope",
        "Sinks",
        &[
            param(
                "Floating",
                "Floating",
                Boolean,
                "off",
                true,
                dialog(
                    "Ports",
                    "A floating scope has no wired input ports; the current simulation backend may not support that mode.",
                    None,
                ),
            ),
            param(
                "NumInputPorts",
                "Number of input ports",
                Int,
                "1",
                true,
                dialog(
                        "Ports",
                        "Number of wired signals accepted by the nonfloating scope.",
                        None,
                    )
                    .when(Visibility::Equals {
                        parameter: "Floating",
                        value: "off",
                    }),
            ),

        ],
        PortRule::Scope,
        [30.0, 32.0],
    ),
    block(
        "Inport",
        "Inport",
        "Ports",
        &[
            param(
                "Port",
                "Port number",
                Int,
                "1",
                false,
                dialog(
                    "Ports",
                    "One-based interface port number within the containing system. Renumbering can move boundary connections.",
                    None,
                ),
            ),
        ],
        SOURCE,
        [30.0, 14.0],
    ),
    block(
        "Outport",
        "Outport",
        "Ports",
        &[
            param(
                "Port",
                "Port number",
                Int,
                "1",
                false,
                dialog(
                    "Ports",
                    "One-based interface port number within the containing system. Renumbering can move boundary connections.",
                    None,
                ),
            ),
        ],
        SINK,
        [30.0, 14.0],
    ),
    block(
        "Mux",
        "Mux",
        "Routing",
        &[
            param(
                "Inputs",
                "Inputs",
                Expr,
                "2",
                true,
                dialog(
                    "Ports",
                    "Number of inputs or a literal vector of input widths; -1 entries inherit width. Changes the input port count.",
                    None,
                ),
            ),
        ],
        PortRule::Count {
            parameter: "Inputs",
            input: true,
            widths: true,
            other: 1,
        },
        [5.0, 38.0],
    ),
    block(
        "Demux",
        "Demux",
        "Routing",
        &[
            param(
                "Outputs",
                "Outputs",
                Expr,
                "2",
                true,
                dialog(
                    "Ports",
                    "Number of outputs or a literal vector of output widths; -1 entries inherit width. Changes the output port count.",
                    None,
                ),
            ),
        ],
        PortRule::Count {
            parameter: "Outputs",
            input: false,
            widths: true,
            other: 1,
        },
        [5.0, 38.0],
    ),
    block(
        "Step",
        "Step",
        "Sources",
        &[
            param(
                "Time",
                "Step time",
                Expr,
                "1",
                false,
                dialog(
                    "Timing",
                    "Simulation time when the output changes from its initial value to its final value.",
                    Some("s"),
                ),
            ),
            param(
                "Before",
                "Initial value",
                Expr,
                "0",
                false,
                dialog("Signal", "Output value before the step time.", None),
            ),
            param(
                "After",
                "Final value",
                Expr,
                "1",
                false,
                dialog("Signal", "Output value at and after the step time.", None),
            ),
            param(
                "SampleTime",
                "Sample time",
                Expr,
                "0",
                false,
                dialog(
                    "Timing",
                    "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                    Some("s"),
                ),
            ),
        ],
        SOURCE,
        [30.0, 30.0],
    ),
    block(
        "Sin",
        "Sine Wave",
        "Sources",
        &[
            param(
                "SineType",
                "Sine type",
                Enum(&["Time based", "Sample based"]),
                "Time based",
                true,
                dialog(
                        "Timing",
                        "Choose a waveform defined by simulation time or by discrete sample position.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "Time based",
                                label: "Time based",
                            },
                            EnumLabel {
                                value: "Sample based",
                                label: "Sample based",
                            },
                        ],
                    ),
            ),
            param(
                "TimeSource",
                "Time source",
                Enum(&["Use simulation time", "Use external signal"]),
                "Use simulation time",
                true,
                dialog(
                        "Timing",
                        "Use simulation time or an external signal as the time input; an external source adds an input port.",
                        None,
                    )
                    .with_enum_labels(
                        &[
                            EnumLabel {
                                value: "Use simulation time",
                                label: "Simulation time",
                            },
                            EnumLabel {
                                value: "Use external signal",
                                label: "External time input",
                            },
                        ],
                    )
                    .when(Visibility::Equals {
                        parameter: "SineType",
                        value: "Time based",
                    }),
            ),
            param(
                "Amplitude",
                "Amplitude",
                Expr,
                "1",
                false,
                dialog(
                    "Signal",
                    "Peak amplitude of the sine component before adding the bias.",
                    None,
                ),
            ),
            param(
                "Bias",
                "Bias",
                Expr,
                "0",
                false,
                dialog("Signal", "Constant offset added to the sine component.", None),
            ),
            param(
                "Frequency",
                "Frequency (rad/s)",
                Expr,
                "1",
                false,
                dialog(
                        "Signal",
                        "Angular frequency of a time-based sine wave.",
                        Some("rad/s"),
                    )
                    .when(Visibility::Equals {
                        parameter: "SineType",
                        value: "Time based",
                    }),
            ),
            param(
                "Phase",
                "Phase (rad)",
                Expr,
                "0",
                false,
                dialog(
                        "Signal",
                        "Initial phase angle of a time-based sine wave.",
                        Some("rad"),
                    )
                    .when(Visibility::Equals {
                        parameter: "SineType",
                        value: "Time based",
                    }),
            ),
            param(
                "Samples",
                "Samples per period",
                Expr,
                "10",
                false,
                dialog(
                        "Signal",
                        "Number of samples in each cycle; enter an integer scalar or vector.",
                        Some("samples"),
                    )
                    .when(Visibility::Equals {
                        parameter: "SineType",
                        value: "Sample based",
                    }),
            ),
            param(
                "Offset",
                "Offset samples",
                Expr,
                "0",
                false,
                dialog(
                        "Signal",
                        "Discrete phase offset measured in sample intervals.",
                        Some("samples"),
                    )
                    .when(Visibility::Equals {
                        parameter: "SineType",
                        value: "Sample based",
                    }),
            ),
            param(
                "SampleTime",
                "Sample time",
                Expr,
                "0",
                false,
                dialog(
                    "Timing",
                    "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                    Some("s"),
                ),
            ),
        ],
        PortRule::SineWave,
        [30.0, 30.0],
    ),
    block("Clock", "Clock", "Sources", &[], SOURCE, [30.0, 30.0]),
    block("Ground", "Ground", "Sources", &[], SOURCE, [30.0, 20.0]),
    block(
        "RandomNumber",
        "Random Number",
        "Sources",
        &[
            param(
                "Mean",
                "Mean",
                Expr,
                "0",
                false,
                dialog(
                    "Distribution",
                    "Mean of the normally distributed random output.",
                    None,
                ),
            ),
            param(
                "Variance",
                "Variance",
                Expr,
                "1",
                false,
                dialog(
                    "Distribution",
                    "Nonnegative variance of the normally distributed random output.",
                    None,
                ),
            ),
            param(
                "Seed",
                "Seed",
                Expr,
                "0",
                false,
                dialog(
                    "Distribution",
                    "Seed expression used to initialize the random sequence; the same seed makes runs reproducible.",
                    None,
                ),
            ),
            param(
                "SampleTime",
                "Sample time",
                Expr,
                "0.1",
                false,
                dialog(
                    "Timing",
                    "Sample period in seconds; -1 inherits timing, 0 is continuous where supported, and inf denotes a constant sample time.",
                    Some("s"),
                ),
            ),
        ],
        SOURCE,
        [40.0, 30.0],
    ),
    block(
        "Saturate",
        "Saturation",
        "Discontinuities",
        &[
            param(
                "UpperLimit",
                "Upper limit",
                Expr,
                "0.5",
                false,
                dialog(
                    "Limits",
                    "Largest permitted output value; must be no smaller than the lower limit.",
                    None,
                ),
            ),
            param(
                "LowerLimit",
                "Lower limit",
                Expr,
                "-0.5",
                false,
                dialog(
                    "Limits",
                    "Smallest permitted output value; must be no greater than the upper limit.",
                    None,
                ),
            ),
        ],
        ONE,
        [30.0, 30.0],
    ),
    block(
        "TransferFcn",
        "Transfer Function",
        "Continuous",
        &[
            param(
                "Numerator",
                "Numerator coefficients",
                Expr,
                "[1]",
                false,
                dialog(
                    "Coefficients",
                    "Numerator coefficients in descending powers of s.",
                    None,
                ),
            ),
            param(
                "Denominator",
                "Denominator coefficients",
                Expr,
                "[1 1]",
                false,
                dialog(
                    "Coefficients",
                    "Denominator coefficients in descending powers of s; the leading coefficient must be nonzero.",
                    None,
                ),
            ),
        ],
        ONE,
        [80.0, 40.0],
    ),
    block(
        "StateSpace",
        "State-Space",
        "Continuous",
        &[
            param(
                "A",
                "State matrix A",
                Expr,
                "1",
                false,
                dialog(
                    "Matrices",
                    "State matrix in the continuous-time equation dx/dt = A*x + B*u.",
                    None,
                ),
            ),
            param(
                "B",
                "Input matrix B",
                Expr,
                "1",
                false,
                dialog(
                    "Matrices",
                    "Input matrix in the continuous-time equation dx/dt = A*x + B*u.",
                    None,
                ),
            ),
            param(
                "C",
                "Output matrix C",
                Expr,
                "1",
                false,
                dialog("Matrices", "Output matrix in the equation y = C*x + D*u.", None),
            ),
            param(
                "D",
                "Feedthrough matrix D",
                Expr,
                "1",
                false,
                dialog(
                    "Matrices",
                    "Direct-feedthrough matrix in the equation y = C*x + D*u.",
                    None,
                ),
            ),
            param(
                "InitialCondition",
                "Initial conditions",
                Expr,
                "0",
                false,
                dialog(
                    "State",
                    "Value of the state before the first update; enter a MATLAB scalar or array expression.",
                    None,
                ),
            ),
            param(
                "AllowTunableDMatrix",
                "Allow tunable D matrix",
                Boolean,
                "off",
                false,
                dialog(
                    "Matrices",
                    "Allow the direct-feedthrough matrix to change during simulation; support depends on the simulation backend.",
                    None,
                ),
            ),
        ],
        ONE,
        [80.0, 50.0],
    ),
    block(
        "Switch",
        "Switch",
        "Routing",
        &[
            ParameterDescriptor {
                implicit_default: Some("u2 >= Threshold"),
                ..param(
                    "Criteria",
                    "Switch criteria",
                    Enum(&["u2 >= Threshold", "u2 > Threshold", "u2 ~= 0"]),
                    "u2 ~= 0",
                    false,
                    dialog(
                            "Selection",
                            "Select the first data input when the control input u2 satisfies this condition; otherwise select the third input.",
                            None,
                        )
                        .with_enum_labels(
                            &[
                                EnumLabel {
                                    value: "u2 >= Threshold",
                                    label: "Control >= threshold",
                                },
                                EnumLabel {
                                    value: "u2 > Threshold",
                                    label: "Control > threshold",
                                },
                                EnumLabel {
                                    value: "u2 ~= 0",
                                    label: "Control is nonzero",
                                },
                            ],
                        ),
                )
            },
            param(
                "Threshold",
                "Threshold",
                Expr,
                "0",
                false,
                dialog(
                        "Selection",
                        "Comparison value for threshold-based switching; unused by the nonzero-control criterion.",
                        None,
                    )
                    .when(Visibility::NotEquals {
                        parameter: "Criteria",
                        value: "u2 ~= 0",
                    }),
            ),
            ParameterDescriptor {
                implicit_default: Some("on"),
                ..param(
                    "ZeroCross",
                    "Zero-crossing detection",
                    Boolean,
                    "off",
                    false,
                    dialog(
                        "Timing",
                        "Request zero-crossing detection for switching events; solver support is required.",
                        None,
                    ),
                )
            },
        ],
        PortRule::Fixed {
            inputs: 3,
            outputs: 1,
        },
        [30.0, 50.0],
    ),
    block("Abs", "Absolute Value", "Math", &[], ONE, [30.0, 30.0]),
    block(
        "Trigonometry",
        "Trigonometric Function",
        "Math",
        &[
            param(
                "Operator",
                "Function",
                Enum(
                    &[
                        "sin",
                        "cos",
                        "tan",
                        "asin",
                        "acos",
                        "atan",
                        "atan2",
                        "sinh",
                        "cosh",
                        "tanh",
                        "asinh",
                        "acosh",
                        "atanh",
                        "sincos",
                        "cos + jsin",
                    ],
                ),
                "sin",
                true,
                dialog(
                    "Operation",
                    "Trigonometric function to apply. atan2 uses two inputs; sincos produces separate sine and cosine outputs.",
                    None,
                ),
            ),
        ],
        PortRule::Trigonometry,
        [40.0, 30.0],
    ),
    block("Terminator", "Terminator", "Sinks", &[], SINK, [20.0, 20.0]),
    block(
        "Display",
        "Display",
        "Sinks",
        &[
            param(
                "Floating",
                "Floating",
                Boolean,
                "off",
                true,
                dialog(
                    "Ports",
                    "A floating display has no wired input port. Turning this on changes the block interface.",
                    None,
                ),
            ),
        ],
        PortRule::Display,
        [60.0, 30.0],
    ),
];

/// Stable palette group order. Block order within each group follows BLOCKS.
pub const PALETTE_CATEGORIES: &[&str] = &[
    "Sources",
    "Math",
    "Continuous",
    "Discrete",
    "Discontinuities",
    "Routing",
    "Ports",
    "Sinks",
];

pub fn blocks_in_category(category: &str) -> impl Iterator<Item = &'static BlockDescriptor> + '_ {
    BLOCKS
        .iter()
        .filter(move |b| b.creatable && b.category == category)
}

pub fn find(type_key: &str) -> Option<&'static BlockDescriptor> {
    BLOCKS.iter().find(|b| b.type_key == type_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ports(ty: &str, key: &str, value: &str) -> PortResolution {
        find(ty)
            .unwrap()
            .resolve_ports(&BTreeMap::from([(key.into(), value.into())]))
    }
    #[test]
    fn creation_profile_has_unique_keys_valid_parameters_and_ports() {
        let mut keys = std::collections::BTreeSet::new();
        for descriptor in BLOCKS {
            assert!(keys.insert(descriptor.type_key));
            for param in descriptor.parameters {
                validate_parameter(param, param.default).unwrap();
            }
            assert!(
                matches!(
                    descriptor.resolve_ports(&descriptor.creation_parameters()),
                    PortResolution::Known(_)
                ),
                "{}",
                descriptor.type_key
            );
        }
        assert_eq!(ports("Scope", "NumInputPorts", "3"), known(3, 0));
    }
    #[test]
    fn signs_counts_and_width_vectors_have_distinct_semantics() {
        assert_eq!(ports("Sum", "Inputs", "|+-+"), known(3, 1));
        assert_eq!(ports("Product", "Inputs", "*/"), known(2, 1));
        assert_eq!(ports("Mux", "Inputs", "3"), known(3, 1));
        assert_eq!(ports("Mux", "Inputs", "[3]"), known(3, 1));
        assert_eq!(ports("Demux", "Outputs", "[2 3]"), known(1, 2));
        for value in ["0", "-1", "2.5", "inf", "NaN", "1025"] {
            assert!(
                matches!(
                    ports("Mux", "Inputs", value),
                    PortResolution::Invalid { .. }
                ),
                "{value}"
            );
        }
    }
    #[test]
    fn expressions_and_absent_values_are_never_guessed() {
        for value in ["n", "numel(K)", "[n 2]", "1+2"] {
            assert!(
                matches!(
                    ports("Mux", "Inputs", value),
                    PortResolution::Unresolved { .. }
                ),
                "{value}"
            );
        }
        assert_eq!(
            find("Sum").unwrap().resolve_ports(&BTreeMap::new()),
            known(2, 1)
        );
        assert_eq!(ports("Integrator", "ExternalReset", "rising"), known(2, 1));
        assert!(find("ThirdPartyBlock").is_none());
    }
    #[test]
    fn editable_integrator_configurations_include_control_and_state_ports() {
        let desc = find("Integrator").unwrap();
        for reset in ["none", "rising", "falling", "either", "level", "level hold"] {
            for external in [false, true] {
                for limited in [false, true] {
                    for saturation in [false, true] {
                        let p = BTreeMap::from([
                            ("ExternalReset".into(), reset.into()),
                            (
                                "InitialConditionSource".into(),
                                if external { "external" } else { "internal" }.into(),
                            ),
                            (
                                "LimitOutput".into(),
                                if limited { "on" } else { "off" }.into(),
                            ),
                            (
                                "ShowSaturationPort".into(),
                                if saturation { "on" } else { "off" }.into(),
                            ),
                            ("ShowStatePort".into(), "on".into()),
                        ]);
                        assert_eq!(
                            desc.resolve_ports(&p),
                            PortResolution::Known(PortCounts {
                                inputs: 1 + u32::from(reset != "none") + u32::from(external),
                                outputs: 1 + u32::from(limited && saturation),
                                state: 1,
                                ..Default::default()
                            })
                        );
                    }
                }
            }
        }
        assert_eq!(ports("Scope", "Floating", "on"), known(0, 0));
    }
    #[test]
    fn width_vectors_and_sign_errors_are_resolved_without_evaluation() {
        for value in ["[1 -1]", "[1;-1]", "[2;3]"] {
            assert_eq!(ports("Mux", "Inputs", value), known(2, 1));
            assert_eq!(ports("Demux", "Outputs", value), known(1, 2));
        }
        for (ty, value) in [
            ("Sum", "**"),
            ("Product", "+-"),
            ("Product", "*|/"),
            ("Mux", "[1 2;3 4]"),
        ] {
            assert!(matches!(
                ports(ty, "Inputs", value),
                PortResolution::Invalid { .. }
            ));
        }
    }
    #[test]
    fn check_edit_accepts_valid_but_not_simulatable_options() {
        let empty = BTreeMap::new();
        for mode in ["Matrix(u*K)", "Matrix(K*u) (u vector)"] {
            assert!(find("Gain")
                .unwrap()
                .check_edit(&empty, "Multiplication", mode)
                .is_ok());
        }
        assert!(find("Product")
            .unwrap()
            .check_edit(&empty, "Multiplication", "Matrix(*)")
            .is_ok());
        for value in ["0", "-3", "5000"] {
            assert!(find("Scope")
                .unwrap()
                .check_edit(&empty, "NumInputPorts", value)
                .is_err());
        }
        let edit = find("Mux")
            .unwrap()
            .check_edit(&empty, "Inputs", "3")
            .unwrap();
        assert_eq!(edit.changes_ports, Some(true));
        assert_eq!(edit.ports, known(3, 1));
        assert!(empty.is_empty());
    }
    #[test]
    fn edit_reports_compare_counts_and_allow_unrelated_repairs() {
        let sum = find("Sum").unwrap();
        let current = BTreeMap::from([("Inputs".into(), "++".into())]);
        assert_eq!(
            sum.check_edit(&current, "Inputs", "-+")
                .unwrap()
                .changes_ports,
            Some(false)
        );
        assert_eq!(
            sum.check_edit(&current, "Inputs", "+++")
                .unwrap()
                .changes_ports,
            Some(true)
        );
        assert_eq!(
            sum.check_edit(&current, "Inputs", "n")
                .unwrap()
                .changes_ports,
            None
        );
        let invalid = BTreeMap::from([("Inputs".into(), "|".into())]);
        for (name, value) in [("IconShape", "round"), ("OutDataTypeStr", "double")] {
            let edit = sum.check_edit(&invalid, name, value).unwrap();
            assert_eq!(edit.changes_ports, Some(false));
            assert_eq!(edit.previous_ports, edit.ports);
        }
        let mux = find("Mux").unwrap();
        assert_eq!(
            mux.check_edit(&BTreeMap::new(), "Inputs", "[2 3]")
                .unwrap()
                .changes_ports,
            Some(false)
        );
        assert_eq!(ports("Mux", "Inputs", "[1;]"), known(1, 1));
        assert!(matches!(
            ports("Mux", "Inputs", "**"),
            PortResolution::Invalid { .. }
        ));
    }
    #[test]
    fn palette_groups_cover_every_creatable_block_once() {
        let keys: Vec<_> = PALETTE_CATEGORIES
            .iter()
            .flat_map(|category| blocks_in_category(category))
            .map(|b| b.type_key)
            .collect();
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
        assert_eq!(keys.len(), BLOCKS.iter().filter(|b| b.creatable).count());
        assert!(blocks_in_category("absent").next().is_none());
    }
    #[test]
    fn alternate_sine_and_trigonometry_modes_have_correct_arity() {
        assert_eq!(
            ports("Sin", "TimeSource", "Use external signal"),
            known(1, 1)
        );
        let sample = BTreeMap::from([
            ("SineType".into(), "Sample based".into()),
            ("TimeSource".into(), "Use external signal".into()),
        ]);
        assert_eq!(find("Sin").unwrap().resolve_ports(&sample), known(0, 1));
        assert_eq!(ports("Trigonometry", "Operator", "atan2"), known(2, 1));
        assert_eq!(ports("Trigonometry", "Operator", "sincos"), known(1, 2));
        assert_eq!(ports("Trigonometry", "Operator", "cos + jsin"), known(1, 1));
        assert_eq!(ports("Display", "Floating", "on"), known(0, 0));
    }
}
