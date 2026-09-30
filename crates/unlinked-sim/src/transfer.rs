//! Lower proper SISO transfer functions of order at most 64 to scalar graphs.
//! Continuous blocks use controllable canonical form for descending-power polynomials:
//! x_i'=x_(i+1), x_N'=u-sum(a_(N-i)*x_i),
//! y=b_0*u+sum((b_(N-i)-a_(N-i)*b_0)*x_i), after normalizing a_0.
//! All states start at zero. No pole/zero cancellation or implicit loop solving.
use super::{block_error, Error, Graph, Kind, Node, Wire};
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::Block;

pub(super) const MAX_ORDER: usize = 64;

pub(super) fn parameter(
    block: &Block,
    key: &str,
    default: &str,
    workspace: &BTreeMap<String, f64>,
) -> Result<unlinked_matlab::array_runtime::Value, Error> {
    use unlinked_matlab::array_runtime::{Value, ValueKind};
    let workspace = workspace
        .iter()
        .map(|(name, value)| (name.clone(), Value::scalar(*value)))
        .collect();
    let value = unlinked_matlab::eval_array_expr(block.param(key).unwrap_or(default), &workspace)
        .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
    if value.kind == ValueKind::Character || value.data.iter().any(|v| !v.is_finite()) {
        return Err(block_error(
            &block.id.0,
            format!("{key} requires finite real numeric values"),
        ));
    }
    Ok(value)
}
fn coefficients(
    block: &Block,
    key: &str,
    default: &str,
    workspace: &BTreeMap<String, f64>,
) -> Result<Vec<f64>, Error> {
    let value = parameter(block, key, default, workspace)?;
    if value.rows != 1 || value.cols == 0 || value.cols > MAX_ORDER + 1 {
        return Err(block_error(
            &block.id.0,
            format!(
                "{key} requires a numeric row containing 1 to 65 coefficients (SISO order <=64)"
            ),
        ));
    }
    Ok(value.data)
}
pub(super) fn initial_values(
    block: &Block,
    key: &str,
    default: &str,
    workspace: &BTreeMap<String, f64>,
    order: usize,
) -> Result<Vec<f64>, Error> {
    let value = parameter(block, key, default, workspace)?;
    if value.data.len() == 1 {
        return Ok(vec![value.data[0]; order]);
    }
    if value.data.len() != order || (value.rows != 1 && value.cols != 1 && order != 0) {
        return Err(block_error(
            &block.id.0,
            format!("{key} must be a scalar or a vector with {order} entries"),
        ));
    }
    Ok(value.data)
}

pub(super) struct Builder<'a> {
    pub(super) graph: &'a mut Graph,
    pub(super) reserved: &'a mut BTreeSet<String>,
    pub(super) id: &'a str,
    pub(super) name: &'a str,
}
impl Builder<'_> {
    pub(super) fn node(&mut self, label: &str, kind: Kind) -> String {
        let mut id = format!("{}/$tf-{label}", self.id);
        let mut suffix = 0;
        while !self.reserved.insert(id.clone()) {
            suffix += 1;
            id = format!("{}/$tf-{label}-{suffix}", self.id);
        }
        self.graph.nodes.push(Node {
            id: id.clone(),
            name: format!("{}/{label}", self.name),
            kind,
        });
        id
    }
    pub(super) fn wire(&mut self, source: &str, target: &str, input: usize) {
        self.graph.wires.push(Wire {
            source: source.into(),
            target: target.into(),
            input,
        });
    }
}

/// Returns the generated input node to which original incoming lines must map.
pub(super) fn lower(
    block: &Block,
    workspace: &BTreeMap<String, f64>,
    graph: &mut Graph,
    reserved: &mut BTreeSet<String>,
    name: &str,
) -> Result<String, Error> {
    let id = &block.id.0;
    for key in ["AbsoluteTolerance", "InitialCondition"] {
        if let Some(value) = block.param(key) {
            let allowed = if key == "AbsoluteTolerance" {
                ["auto", "-1"].contains(&value)
            } else {
                value == "0"
            };
            if !allowed {
                return Err(block_error(
                    id,
                    format!("unsupported TransferFcn {key}={value}"),
                ));
            }
        }
    }
    let denominator = coefficients(block, "Denominator", "[1 1]", workspace)?;
    let numerator = coefficients(block, "Numerator", "[1]", workspace)?;
    if denominator[0] == 0.0 || numerator.len() > denominator.len() {
        return Err(block_error(
            id,
            "TransferFcn requires a proper numerator and nonzero leading denominator",
        ));
    }
    let a: Vec<_> = denominator.iter().map(|v| v / denominator[0]).collect();
    let mut b = vec![0.0; a.len()];
    let offset = b.len() - numerator.len();
    for (i, value) in numerator.iter().enumerate() {
        b[offset + i] = value / denominator[0];
    }
    if a.iter().chain(&b).any(|v| !v.is_finite()) {
        return Err(block_error(id, "transfer normalization overflow"));
    }
    if graph.nodes.len().saturating_add(3 * (a.len() - 1) + 4) > 100_000 {
        return Err(Error::Options("lowered graph budget exceeded".into()));
    }
    let mut builder = Builder {
        graph,
        reserved,
        id,
        name,
    };
    let input = builder.node("input", Kind::Sink);
    let order = a.len() - 1;
    let mut states = Vec::new();
    for i in 0..order {
        states.push(builder.node(
            &format!("state{}", i + 1),
            Kind::Integrator { initial: 0.0 },
        ));
    }
    if order > 0 {
        let derivative = builder.node(
            "derivative",
            Kind::Sum {
                signs: vec![1.0; order + 1],
            },
        );
        builder.wire(&input, &derivative, 0);
        for (i, state) in states.iter().enumerate() {
            let gain = builder.node(
                &format!("feedback{}", i + 1),
                Kind::Gain {
                    gain: -a[order - i],
                },
            );
            builder.wire(state, &gain, 0);
            builder.wire(&gain, &derivative, i + 1);
            if i + 1 < order {
                builder.wire(&states[i + 1], state, 0);
            }
        }
        builder.wire(&derivative, states.last().unwrap(), 0);
    }
    let mut terms = Vec::new();
    for (i, state) in states.iter().enumerate() {
        let gain = b[order - i] - a[order - i] * b[0];
        if !gain.is_finite() {
            return Err(block_error(id, "transfer realization overflow"));
        }
        if gain != 0.0 {
            let output = builder.node(&format!("output{}", i + 1), Kind::Gain { gain });
            builder.wire(state, &output, 0);
            terms.push(output);
        }
    }
    if b[0] != 0.0 {
        let direct = builder.node("feedthrough", Kind::Gain { gain: b[0] });
        builder.wire(&input, &direct, 0);
        terms.push(direct);
    }
    let kind = if terms.is_empty() {
        Kind::Constant { value: 0.0 }
    } else {
        Kind::Sum {
            signs: vec![1.0; terms.len()],
        }
    };
    builder.graph.nodes.push(Node {
        id: id.clone(),
        name: name.into(),
        kind,
    });
    for (i, term) in terms.iter().enumerate() {
        builder.wire(term, id, i);
    }
    Ok(input)
}

/// Direct-form-II discrete realization. States hold w[k-1], w[k-2], ...,
/// where w[k]=(u[k]-sum(a_i*w[k-i]))/a_0. Numerator polynomials use
/// descending powers of z, so shorter numerator rows are padded on the left.
/// Raw (unnormalized) coefficients preserve Simulink's initial-state meaning.
/// Reference: <https://www.mathworks.com/help/simulink/slref/discretetransferfcn.html>.
pub(super) fn lower_discrete(
    block: &Block,
    workspace: &BTreeMap<String, f64>,
    graph: &mut Graph,
    reserved: &mut BTreeSet<String>,
    name: &str,
) -> Result<(String, bool), Error> {
    let id = &block.id.0;
    for (key, allowed) in [
        ("NumeratorSource", &["Dialog"][..]),
        ("DenominatorSource", &["Dialog"][..]),
        ("InitialStatesSource", &["Dialog"][..]),
        ("ExternalReset", &["None", "none"][..]),
        (
            "InputProcessing",
            &["Elements as channels (sample based)"][..],
        ),
        ("a0EqualsOne", &["off", "on"][..]),
    ] {
        if block
            .param(key)
            .is_some_and(|v| !allowed.contains(&v.trim()))
        {
            return Err(block_error(
                id,
                format!("unsupported DiscreteTransferFcn {key}"),
            ));
        }
    }
    for key in [
        "StateDataTypeStr",
        "NumCoefDataTypeStr",
        "DenCoefDataTypeStr",
        "NumProductDataTypeStr",
        "DenProductDataTypeStr",
        "NumAccumDataTypeStr",
        "DenAccumDataTypeStr",
    ] {
        if block.param(key).is_some_and(|v| {
            ![
                "double",
                "Inherit: Same as input",
                "Inherit: Same as product output",
                "Inherit: Inherit via internal rule",
            ]
            .contains(&v.trim())
        }) {
            return Err(block_error(
                id,
                format!("non-double DiscreteTransferFcn {key} unsupported"),
            ));
        }
    }
    let a = coefficients(block, "Denominator", "[1 0.5]", workspace)?;
    let numerator = coefficients(block, "Numerator", "[1]", workspace)?;
    if a[0] == 0.0 || numerator.len() > a.len() {
        return Err(block_error(
            id,
            "DiscreteTransferFcn requires a proper numerator and nonzero leading denominator",
        ));
    }
    if block.param("a0EqualsOne") == Some("on") && a[0] != 1.0 {
        return Err(block_error(
            id,
            "a0EqualsOne requires leading denominator coefficient exactly one",
        ));
    }
    let inverse = 1.0 / a[0];
    if !inverse.is_finite() {
        return Err(block_error(
            id,
            "discrete denominator normalization overflow",
        ));
    }
    let order = a.len() - 1;
    let initial = initial_values(block, "InitialStates", "0", workspace, order)?;
    let mut b = vec![0.0; a.len()];
    let offset = b.len() - numerator.len();
    b[offset..].copy_from_slice(&numerator);
    if graph.nodes.len().saturating_add(3 * order + 5) > 100_000 {
        return Err(Error::Options("lowered graph budget exceeded".into()));
    }
    let mut builder = Builder {
        graph,
        reserved,
        id,
        name,
    };
    let input = builder.node("input", Kind::Sink);
    let states: Vec<_> = initial
        .into_iter()
        .enumerate()
        .map(|(i, initial)| builder.node(&format!("delay{}", i + 1), Kind::UnitDelay { initial }))
        .collect();
    let mut feedback = vec![input.clone()];
    for (i, state) in states.iter().enumerate() {
        if a[i + 1] != 0.0 {
            let gain = builder.node(
                &format!("feedback{}", i + 1),
                Kind::Gain { gain: -a[i + 1] },
            );
            builder.wire(state, &gain, 0);
            feedback.push(gain);
        }
    }
    let combined = builder.node(
        "denominator",
        Kind::Sum {
            signs: vec![1.0; feedback.len()],
        },
    );
    for (i, source) in feedback.iter().enumerate() {
        builder.wire(source, &combined, i);
    }
    let current = builder.node("current", Kind::Gain { gain: inverse });
    builder.wire(&combined, &current, 0);
    for (i, state) in states.iter().enumerate() {
        builder.wire(if i == 0 { &current } else { &states[i - 1] }, state, 0);
    }
    let mut terms = Vec::new();
    for (i, coefficient) in b.iter().enumerate() {
        if *coefficient != 0.0 {
            let gain = builder.node(&format!("output{i}"), Kind::Gain { gain: *coefficient });
            builder.wire(if i == 0 { &current } else { &states[i - 1] }, &gain, 0);
            terms.push(gain);
        }
    }
    let kind = if terms.is_empty() {
        Kind::Constant { value: 0.0 }
    } else {
        Kind::Sum {
            signs: vec![1.0; terms.len()],
        }
    };
    builder.graph.nodes.push(Node {
        id: id.clone(),
        name: name.into(),
        kind,
    });
    for (i, term) in terms.iter().enumerate() {
        builder.wire(term, id, i);
    }
    Ok((input, b[0] != 0.0))
}

/// A discrete direct-feedthrough output is only evaluated at solver stages;
/// without a hold we cannot faithfully feed it into continuous state derivatives.
/// Walk once from all such outputs, stopping at explicit UnitDelay barriers.
pub(super) fn validate_discrete_coupling(graph: &Graph, sources: &[String]) -> Result<(), Error> {
    use std::collections::VecDeque;
    if graph.nodes.len() > 100_000 || graph.wires.len() > 1_000_000 {
        return Err(Error::Options(
            "discrete coupling graph budget exceeded".into(),
        ));
    }
    let kinds: BTreeMap<_, _> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), &n.kind))
        .collect();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for wire in &graph.wires {
        outgoing.entry(&wire.source).or_default().push(&wire.target);
    }
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    for source in sources {
        if visited.insert(source.as_str()) {
            queue.push_back((source.as_str(), source.as_str()));
        }
    }
    while let Some((node, origin)) = queue.pop_front() {
        match kinds.get(node) {
            Some(Kind::UnitDelay { .. } | Kind::RateDelay { .. }) => continue,
            Some(Kind::Integrator { .. }) => return Err(block_error(origin,
                "direct-feedthrough discrete output reaches continuous state without an explicit UnitDelay hold; this mixed-rate coupling is unsupported")),
            _ => {}
        }
        if let Some(targets) = outgoing.get(node) {
            for target in targets {
                if visited.insert(*target) {
                    queue.push_back((target, origin));
                }
            }
        }
    }
    Ok(())
}
