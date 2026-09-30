//! Lower proper SISO transfer functions of order at most two to scalar graphs.
//! For normalized D(s)=s²+a1*s+a2, use x1'=x2,
//! x2'=u-a2*x1-a1*x2 and y=(b2-a2*b0)*x1+(b1-a1*b0)*x2+b0*u.
//! All states start at zero. No pole/zero cancellation or implicit loop solving.
use super::{block_error, Error, Graph, Kind, Node, Wire};
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::Block;

fn coefficients(
    block: &Block,
    key: &str,
    default: &str,
    workspace: &BTreeMap<String, f64>,
) -> Result<Vec<f64>, Error> {
    let raw = block.param(key).unwrap_or(default).trim();
    if raw.len() > 65_536 {
        return Err(block_error(
            &block.id.0,
            "coefficient expression exceeds size limit",
        ));
    }
    let parts = if let Some(body) = raw.strip_prefix('[') {
        let body = body
            .strip_suffix(']')
            .ok_or_else(|| block_error(&block.id.0, "unclosed coefficient vector"))?;
        if body.contains([';', '[', ']']) {
            return Err(block_error(
                &block.id.0,
                "only a single row of scalar coefficients is supported",
            ));
        }
        let mut depth: usize = 0;
        let mut commas = false;
        for c in body.chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth = depth.checked_sub(1).ok_or_else(|| {
                        block_error(&block.id.0, "unbalanced coefficient expression")
                    })?
                }
                ',' if depth == 0 => commas = true,
                _ => {}
            }
        }
        if depth != 0 {
            return Err(block_error(
                &block.id.0,
                "unbalanced coefficient expression",
            ));
        }
        let mut parts = Vec::new();
        let mut start = 0;
        for (index, c) in body.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 && (c == ',' || (!commas && c.is_whitespace())) {
                let part = body[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                } else if commas {
                    return Err(block_error(&block.id.0, "empty coefficient"));
                }
                start = index + c.len_utf8();
            }
        }
        let last = body[start..].trim();
        if !last.is_empty() {
            parts.push(last);
        } else if commas {
            return Err(block_error(&block.id.0, "empty coefficient"));
        }
        parts
    } else {
        vec![raw]
    };
    if parts.is_empty() || parts.len() > 3 {
        return Err(block_error(
            &block.id.0,
            "TransferFcn supports one to three coefficients only",
        ));
    }
    parts
        .into_iter()
        .map(|part| {
            let value = unlinked_matlab::eval_expr(part, workspace)
                .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
            if !value.is_finite() {
                return Err(block_error(&block.id.0, "non-finite transfer coefficient"));
            }
            Ok(value)
        })
        .collect()
}

struct Builder<'a> {
    graph: &'a mut Graph,
    reserved: &'a mut BTreeSet<String>,
    id: &'a str,
    name: &'a str,
}
impl Builder<'_> {
    fn node(&mut self, label: &str, kind: Kind) -> String {
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
    fn wire(&mut self, source: &str, target: &str, input: usize) {
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
    if graph.nodes.len() + 16 > 100_000 {
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
