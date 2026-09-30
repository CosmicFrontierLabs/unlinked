//! Bounded SISO continuous state-space realization: x'=A*x+B*u, y=C*x+D*u.
//! Only finite, time-invariant real matrices with one to 64 states are lowered.
//! Reference: <https://www.mathworks.com/help/simulink/slref/statespace.html>.
use super::transfer::{initial_values, parameter, Builder, MAX_ORDER};
use super::{block_error, Error, Graph, Kind, Node};
use std::collections::{BTreeMap, BTreeSet};
use unlinked_model::Block;

pub(super) fn lower(
    block: &Block,
    workspace: &BTreeMap<String, f64>,
    graph: &mut Graph,
    reserved: &mut BTreeSet<String>,
    name: &str,
) -> Result<String, Error> {
    let id = &block.id.0;
    if block
        .param("AbsoluteTolerance")
        .is_some_and(|v| !["auto", "-1"].contains(&v.trim()))
    {
        return Err(block_error(id,"per-block StateSpace AbsoluteTolerance is unsupported; choose global solver tolerances"));
    }
    // Enabling tunable D declares direct feedthrough even when D currently zero.
    // Dynamic parameter tuning is not implemented, so reject that mode explicitly.
    if block
        .param("AllowTunableDMatrix")
        .is_some_and(|v| v.trim() != "off")
    {
        return Err(block_error(
            id,
            "tunable StateSpace D matrix is unsupported",
        ));
    }
    let a = parameter(block, "A", "1", workspace)?;
    let b = parameter(block, "B", "1", workspace)?;
    let c = parameter(block, "C", "1", workspace)?;
    let d = parameter(block, "D", "1", workspace)?;
    let order = a.rows;
    if order == 0 || order > MAX_ORDER || a.cols != order {
        return Err(block_error(
            id,
            "StateSpace A must be square with 1 to 64 states",
        ));
    }
    if b.rows != order
        || b.cols != 1
        || c.rows != 1
        || c.cols != order
        || d.rows != 1
        || d.cols != 1
    {
        return Err(block_error(
            id,
            "StateSpace currently requires SISO dimensions: B=N-by-1, C=1-by-N, D=1-by-1",
        ));
    }
    let initial_key = if block.param("InitialCondition").is_some() {
        "InitialCondition"
    } else if block.param("X0").is_some() {
        "X0"
    } else {
        "InitialCondition"
    };
    let initial = initial_values(block, initial_key, "0", workspace, order)?;
    let nonzero = a
        .data
        .iter()
        .chain(&b.data)
        .chain(&c.data)
        .chain(&d.data)
        .filter(|v| **v != 0.0)
        .count();
    if graph.nodes.len().saturating_add(2 * order + nonzero + 2) > 100_000 {
        return Err(Error::Options("lowered graph budget exceeded".into()));
    }
    let mut builder = Builder {
        graph,
        reserved,
        id,
        name,
    };
    let input = builder.node("ss-input", Kind::Sink);
    let states: Vec<_> = initial
        .into_iter()
        .enumerate()
        .map(|(i, initial)| {
            builder.node(&format!("ss-state{}", i + 1), Kind::Integrator { initial })
        })
        .collect();
    for (row, state) in states.iter().enumerate() {
        let mut terms = Vec::new();
        for (column, source) in states.iter().enumerate() {
            let coefficient = a.data[row + column * order];
            if coefficient != 0.0 {
                let gain = builder.node(
                    &format!("ss-a{row}-{column}"),
                    Kind::Gain { gain: coefficient },
                );
                builder.wire(source, &gain, 0);
                terms.push(gain);
            }
        }
        if b.data[row] != 0.0 {
            let gain = builder.node(&format!("ss-b{row}"), Kind::Gain { gain: b.data[row] });
            builder.wire(&input, &gain, 0);
            terms.push(gain);
        }
        let kind = if terms.is_empty() {
            Kind::Constant { value: 0.0 }
        } else {
            Kind::Sum {
                signs: vec![1.0; terms.len()],
            }
        };
        let derivative = builder.node(&format!("ss-derivative{row}"), kind);
        for (i, term) in terms.iter().enumerate() {
            builder.wire(term, &derivative, i);
        }
        builder.wire(&derivative, state, 0);
    }
    let mut terms = Vec::new();
    for (i, state) in states.iter().enumerate() {
        if c.data[i] != 0.0 {
            let gain = builder.node(&format!("ss-c{i}"), Kind::Gain { gain: c.data[i] });
            builder.wire(state, &gain, 0);
            terms.push(gain);
        }
    }
    if d.data[0] != 0.0 {
        let gain = builder.node("ss-feedthrough", Kind::Gain { gain: d.data[0] });
        builder.wire(&input, &gain, 0);
        terms.push(gain);
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
