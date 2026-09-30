//! Creation metadata for a deliberately small native-block palette.
//!
//! Defaults are an Unlinked creation profile, not defaults for imported files.
//! This module never evaluates MATLAB, runs callbacks, or certifies simulation
//! support. Unknown imported types and parameters must remain preservable.
use crate::PortCounts;
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

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParameterDescriptor {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParameterKind,
    pub default: &'static str,
    pub affects_ports: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum PortRule {
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

impl BlockDescriptor {
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
        self.ports.resolve(parameters)
    }
}

impl PortRule {
    pub fn resolve(self, parameters: &BTreeMap<String, String>) -> PortResolution {
        let (parameter, input, widths, signs, other) = match self {
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
                .all(|c| signs.contains(c) || c == '|' || c.is_whitespace())
            {
                Some(raw.chars().filter(|&c| signs.contains(c)).count() as f64)
            } else {
                None
            }
        } else {
            None
        };
        let count = count.or_else(|| raw.parse::<f64>().ok());
        let count = if let Some(count) = count {
            count
        } else if widths && raw.starts_with('[') && raw.ends_with(']') {
            // Only plain numeric row vectors are handled here; expressions,
            // colon notation and concatenations belong to the evaluator.
            let inner = &raw[1..raw.len() - 1];
            if inner.contains(';') {
                return unresolved();
            }
            let mut n = 0;
            let mut only_width = 0.0;
            for token in inner
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
            {
                let Ok(width) = token.parse::<f64>() else {
                    return unresolved();
                };
                if width == -1.0 {
                    return unresolved();
                }
                if !width.is_finite() || width < 1.0 || width.fract() != 0.0 {
                    return invalid();
                }
                only_width = width;
                n += 1;
                if n > MAX_PORTS {
                    return invalid();
                }
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
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err("expected an integer expression".into());
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

const fn param(
    name: &'static str,
    kind: ParameterKind,
    default: &'static str,
    affects_ports: bool,
) -> ParameterDescriptor {
    ParameterDescriptor {
        name,
        label: name,
        kind,
        default,
        affects_ports,
    }
}
const fn block(
    type_key: &'static str,
    label: &'static str,
    category: &'static str,
    parameters: &'static [ParameterDescriptor],
    ports: PortRule,
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
        default_size: [60.0, 40.0],
        creatable: true,
    }
}
use ParameterKind::{Enum, Expression as Expr, IntegerExpression as Int};
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
            param("Value", Expr, "1", false),
            param("SampleTime", Expr, "inf", false),
        ],
        SOURCE,
    ),
    block(
        "Gain",
        "Gain",
        "Math",
        &[
            param("Gain", Expr, "1", false),
            param(
                "Multiplication",
                Enum(&["Element-wise(K.*u)", "Matrix(K*u)"]),
                "Element-wise(K.*u)",
                false,
            ),
        ],
        ONE,
    ),
    block(
        "Sum",
        "Sum",
        "Math",
        &[param("Inputs", Expr, "++", true)],
        PortRule::Signs {
            parameter: "Inputs",
            signs: "+-",
        },
    ),
    block(
        "Product",
        "Product",
        "Math",
        &[
            param("Inputs", Expr, "**", true),
            param(
                "Multiplication",
                Enum(&["Element-wise(.*)"]),
                "Element-wise(.*)",
                false,
            ),
        ],
        PortRule::Signs {
            parameter: "Inputs",
            signs: "*/",
        },
    ),
    block(
        "Integrator",
        "Integrator",
        "Continuous",
        &[
            param("InitialCondition", Expr, "0", false),
            param("ExternalReset", Enum(&["none"]), "none", true),
            param(
                "InitialConditionSource",
                Enum(&["internal"]),
                "internal",
                true,
            ),
        ],
        ONE,
    ),
    block(
        "UnitDelay",
        "Unit Delay",
        "Discrete",
        &[
            param("InitialCondition", Expr, "0", false),
            param("SampleTime", Expr, "-1", false),
        ],
        ONE,
    ),
    block(
        "ZeroOrderHold",
        "Zero-Order Hold",
        "Discrete",
        &[param("SampleTime", Expr, "1", false)],
        ONE,
    ),
    block(
        "Scope",
        "Scope",
        "Sinks",
        &[param("NumInputPorts", Int, "1", true)],
        PortRule::Count {
            parameter: "NumInputPorts",
            input: true,
            widths: false,
            other: 0,
        },
    ),
    block(
        "Inport",
        "Inport",
        "Ports",
        &[param("Port", Int, "1", false)],
        SOURCE,
    ),
    block(
        "Outport",
        "Outport",
        "Ports",
        &[param("Port", Int, "1", false)],
        SINK,
    ),
    block(
        "Mux",
        "Mux",
        "Routing",
        &[param("Inputs", Expr, "2", true)],
        PortRule::Count {
            parameter: "Inputs",
            input: true,
            widths: true,
            other: 1,
        },
    ),
    block(
        "Demux",
        "Demux",
        "Routing",
        &[param("Outputs", Expr, "2", true)],
        PortRule::Count {
            parameter: "Outputs",
            input: false,
            widths: true,
            other: 1,
        },
    ),
];

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
        for value in ["n", "numel(K)", "[n 2]", "[1 -1]", "[1;2]", "1+2"] {
            assert!(
                matches!(
                    ports("Mux", "Inputs", value),
                    PortResolution::Unresolved { .. }
                ),
                "{value}"
            );
        }
        assert!(matches!(
            find("Sum").unwrap().resolve_ports(&BTreeMap::new()),
            PortResolution::Unresolved { .. }
        ));
        assert!(matches!(
            ports("Integrator", "ExternalReset", "rising"),
            PortResolution::Invalid { .. }
        ));
        assert!(find("ThirdPartyBlock").is_none());
    }
}
