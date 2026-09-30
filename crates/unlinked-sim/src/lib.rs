//! Deterministic scalar simulation. Unsupported semantics are errors, never substitutions.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("simulation cancelled")]
    Cancelled,
    #[error("invalid simulation options: {0}")]
    Options(String),
    #[error("block {block}: {message}")]
    Block { block: String, message: String },
    #[error("algebraic loop involving: {0}")]
    AlgebraicLoop(String),
    #[error("invalid connection: {0}")]
    Connection(String),
    #[error("workspace parameter {0}: {1}")]
    Parameter(String, String),
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Solver {
    Euler,
    #[default]
    Rk4,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub start: f64,
    pub stop: f64,
    pub step: f64,
    pub solver: Solver,
    /// Includes the initial sample. Prevents accidental unbounded allocation.
    pub max_samples: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            start: 0.0,
            stop: 10.0,
            step: 0.01,
            solver: Solver::Rk4,
            max_samples: 100_001,
        }
    }
}

/// A compiled, scalar block. Ports in this execution representation are zero-based.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    Constant {
        value: f64,
    },
    Step {
        time: f64,
        before: f64,
        after: f64,
    },
    Sine {
        amplitude: f64,
        frequency: f64,
        phase: f64,
        bias: f64,
    },
    Clock,
    Gain {
        gain: f64,
    },
    Bias {
        bias: f64,
    },
    Sum {
        signs: Vec<f64>,
    },
    Product {
        divide: Vec<bool>,
    },
    Saturation {
        lower: f64,
        upper: f64,
    },
    Integrator {
        initial: f64,
    },
    /// Single-rate only: one tick per requested simulation step.
    UnitDelay {
        initial: f64,
    },
    Abs,
    Unary {
        operation: String,
    },
    Sink,
}
impl Kind {
    fn input_count(&self) -> usize {
        match self {
            Self::Constant { .. } | Self::Step { .. } | Self::Sine { .. } | Self::Clock => 0,
            Self::Sum { signs } => signs.len(),
            Self::Product { divide } => divide.len(),
            _ => 1,
        }
    }
    fn is_state(&self) -> bool {
        matches!(self, Self::Integrator { .. } | Self::UnitDelay { .. })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: Kind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wire {
    pub source: String,
    pub target: String,
    pub input: usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub wires: Vec<Wire>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trace {
    pub time: Vec<f64>,
    /// Stable block IDs, including sinks, mapped to samples at `time`.
    pub signals: BTreeMap<String, Vec<f64>>,
    pub solver: Solver,
}

struct Compiled<'a> {
    graph: &'a Graph,
    inputs: Vec<Vec<usize>>,
    order: Vec<usize>,
    states: Vec<usize>,
}
fn block_error(id: &str, message: impl Into<String>) -> Error {
    Error::Block {
        block: id.to_owned(),
        message: message.into(),
    }
}
impl<'a> Compiled<'a> {
    fn new(graph: &'a Graph) -> Result<Self, Error> {
        let mut ids = BTreeMap::new();
        for (i, node) in graph.nodes.iter().enumerate() {
            if ids.insert(node.id.as_str(), i).is_some() {
                return Err(block_error(&node.id, "duplicate block ID"));
            }
            let values: Vec<f64> = match &node.kind {
                Kind::Constant { value } => vec![*value],
                Kind::Step {
                    time,
                    before,
                    after,
                } => vec![*time, *before, *after],
                Kind::Sine {
                    amplitude,
                    frequency,
                    phase,
                    bias,
                } => vec![*amplitude, *frequency, *phase, *bias],
                Kind::Gain { gain } => vec![*gain],
                Kind::Bias { bias } => vec![*bias],
                Kind::Sum { signs } => signs.clone(),
                Kind::Saturation { lower, upper } => {
                    if lower > upper {
                        return Err(block_error(&node.id, "lower limit exceeds upper limit"));
                    }
                    vec![*lower, *upper]
                }
                Kind::Integrator { initial } | Kind::UnitDelay { initial } => vec![*initial],
                _ => vec![],
            };
            if values.iter().any(|v| !v.is_finite()) {
                return Err(block_error(&node.id, "non-finite parameter"));
            }
            if let Kind::Unary { operation } = &node.kind {
                if !["sin", "cos", "tan", "exp", "log", "sqrt"].contains(&operation.as_str()) {
                    return Err(block_error(
                        &node.id,
                        format!("unsupported unary operation {operation}"),
                    ));
                }
            }
        }
        let mut inputs: Vec<Vec<Option<usize>>> = graph
            .nodes
            .iter()
            .map(|n| vec![None; n.kind.input_count()])
            .collect();
        for wire in &graph.wires {
            let source = *ids
                .get(wire.source.as_str())
                .ok_or_else(|| Error::Connection(format!("unknown source {}", wire.source)))?;
            let target = *ids
                .get(wire.target.as_str())
                .ok_or_else(|| Error::Connection(format!("unknown target {}", wire.target)))?;
            let input = inputs[target].get_mut(wire.input).ok_or_else(|| {
                Error::Connection(format!("{} has no input {}", wire.target, wire.input + 1))
            })?;
            if input.replace(source).is_some() {
                return Err(Error::Connection(format!(
                    "multiple drivers of {} input {}",
                    wire.target,
                    wire.input + 1
                )));
            }
        }
        let inputs: Vec<Vec<usize>> = inputs
            .into_iter()
            .enumerate()
            .map(|(i, ports)| {
                ports
                    .into_iter()
                    .enumerate()
                    .map(|(p, v)| {
                        v.ok_or_else(|| {
                            block_error(&graph.nodes[i].id, format!("unconnected input {}", p + 1))
                        })
                    })
                    .collect()
            })
            .collect::<Result<_, _>>()?;
        let states: Vec<usize> = graph
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.kind.is_state().then_some(i))
            .collect();
        let mut dependents = vec![vec![]; graph.nodes.len()];
        let mut remaining = vec![0; graph.nodes.len()];
        let mut ready = BTreeSet::new();
        for (i, node) in graph.nodes.iter().enumerate() {
            if !node.kind.is_state() {
                remaining[i] = inputs[i].len();
                for &source in &inputs[i] {
                    dependents[source].push(i);
                }
            }
            if remaining[i] == 0 {
                ready.insert(i);
            }
        }
        let mut order = vec![];
        let mut visited = 0;
        while let Some(i) = ready.pop_first() {
            visited += 1;
            if !graph.nodes[i].kind.is_state() {
                order.push(i);
            }
            for &target in &dependents[i] {
                remaining[target] -= 1;
                if remaining[target] == 0 {
                    ready.insert(target);
                }
            }
        }
        if visited != graph.nodes.len() {
            return Err(Error::AlgebraicLoop(
                graph
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| remaining[*i] > 0)
                    .map(|(_, n)| n.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        Ok(Self {
            graph,
            inputs,
            order,
            states,
        })
    }
    fn evaluate(&self, t: f64, state: &[f64]) -> Result<Vec<f64>, Error> {
        let mut values = vec![0.0; self.graph.nodes.len()];
        for (s, i) in self.states.iter().enumerate() {
            values[*i] = state[s];
        }
        for &i in &self.order {
            let x = |port: usize| values[self.inputs[i][port]];
            let value = match &self.graph.nodes[i].kind {
                Kind::Constant { value } => *value,
                Kind::Step {
                    time,
                    before,
                    after,
                } => {
                    if t < *time
                        && (*time - t) > 8.0 * f64::EPSILON * time.abs().max(t.abs()).max(1.0)
                    {
                        *before
                    } else {
                        *after
                    }
                }
                Kind::Sine {
                    amplitude,
                    frequency,
                    phase,
                    bias,
                } => amplitude * (frequency * t + phase).sin() + bias,
                Kind::Clock => t,
                Kind::Gain { gain } => gain * x(0),
                Kind::Bias { bias } => x(0) + bias,
                Kind::Sum { signs } => signs.iter().enumerate().map(|(p, s)| s * x(p)).sum(),
                Kind::Product { divide } => {
                    divide
                        .iter()
                        .enumerate()
                        .fold(1.0, |a, (p, d)| if *d { a / x(p) } else { a * x(p) })
                }
                Kind::Saturation { lower, upper } => x(0).clamp(*lower, *upper),
                Kind::Abs => x(0).abs(),
                Kind::Unary { operation } => match operation.as_str() {
                    "sin" => x(0).sin(),
                    "cos" => x(0).cos(),
                    "tan" => x(0).tan(),
                    "exp" => x(0).exp(),
                    "log" => x(0).ln(),
                    "sqrt" => x(0).sqrt(),
                    _ => unreachable!(),
                },
                Kind::Sink => x(0),
                Kind::Integrator { .. } | Kind::UnitDelay { .. } => unreachable!(),
            };
            if !value.is_finite() {
                return Err(block_error(
                    &self.graph.nodes[i].id,
                    format!("non-finite signal at time {t}"),
                ));
            }
            values[i] = value;
        }
        Ok(values)
    }
    fn derivative(&self, values: &[f64]) -> Vec<f64> {
        self.states
            .iter()
            .map(|&i| {
                if matches!(self.graph.nodes[i].kind, Kind::Integrator { .. }) {
                    values[self.inputs[i][0]]
                } else {
                    0.0
                }
            })
            .collect()
    }
}

/// Execute a bounded fixed-step scalar graph. Integrators are simultaneous, delays update
/// only after every continuous solver stage, and algebraic loops are rejected before running.
pub fn simulate(graph: &Graph, options: &Options) -> Result<Trace, Error> {
    simulate_with_observer(graph, options, |_| true)
}

/// One output sample borrowed from the execution engine. Values follow `nodes`
/// order; observers may copy selected signals into bounded streaming buffers.
pub struct Sample<'a> {
    pub time: f64,
    pub nodes: &'a [Node],
    pub values: &'a [f64],
}

/// Run with a synchronous sample observer. Returning false cancels execution.
/// The observer runs on the caller's thread; async servers should use a bounded
/// worker queue and apply backpressure rather than running this on an executor.
pub fn simulate_with_observer(
    graph: &Graph,
    options: &Options,
    mut observer: impl FnMut(Sample<'_>) -> bool,
) -> Result<Trace, Error> {
    let o = options;
    if !o.start.is_finite()
        || !o.stop.is_finite()
        || !o.step.is_finite()
        || o.step <= 0.0
        || o.stop < o.start
    {
        return Err(Error::Options(
            "require finite start <= stop and positive finite step".into(),
        ));
    }
    let ticks = (o.stop - o.start) / o.step;
    let near_integer = (ticks == 0.0 || ticks.round() >= 1.0)
        && (ticks - ticks.round()).abs() <= 8.0 * f64::EPSILON * ticks.abs().max(1.0);
    let intervals = if near_integer {
        ticks.round()
    } else {
        ticks.ceil()
    };
    if !intervals.is_finite() || intervals >= o.max_samples as f64 || o.max_samples > 1_000_001 {
        return Err(Error::Options(
            "sample budget exceeded (hard limit 1,000,001)".into(),
        ));
    }
    let count = intervals as usize + 1;
    if graph.nodes.len() > 100_000 || graph.wires.len() > 1_000_000 {
        return Err(Error::Options("graph budget exceeded".into()));
    }
    if graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, Kind::UnitDelay { .. }))
    {
        let ticks = (o.stop - o.start) / o.step;
        if (ticks - ticks.round()).abs() > 1e-9 {
            return Err(Error::Options(
                "UnitDelay requires stop time on the fixed-step grid".into(),
            ));
        }
    }
    if graph.nodes.len().saturating_mul(count) > 10_000_000 {
        return Err(Error::Options(
            "signal output budget exceeded (10 million values)".into(),
        ));
    }
    for node in &graph.nodes {
        if let Kind::Step { time, .. } = node.kind {
            let ticks = (time - o.start) / o.step;
            if time > o.start && time < o.stop && (ticks - ticks.round()).abs() > 1e-9 {
                return Err(block_error(
                    &node.id,
                    "Step transition must align with fixed-step grid",
                ));
            }
        }
    }
    // Accepted decimal transition times are snapped to the same grid used for
    // samples; validation tolerance must not create inconsistent stage values.
    let mut normalized = graph.clone();
    for node in &mut normalized.nodes {
        if let Kind::Step { time, .. } = &mut node.kind {
            if *time >= o.start
                && *time <= o.stop
                && (((*time - o.start) / o.step) - ((*time - o.start) / o.step).round()).abs()
                    <= 1e-9
            {
                *time = o.start + ((*time - o.start) / o.step).round() * o.step;
            }
        }
    }
    let graph = &normalized;
    let compiled = Compiled::new(graph)?;
    let mut state: Vec<f64> = compiled
        .states
        .iter()
        .map(|&i| match graph.nodes[i].kind {
            Kind::Integrator { initial } | Kind::UnitDelay { initial } => initial,
            _ => unreachable!(),
        })
        .collect();
    let mut trace = Trace {
        time: Vec::with_capacity(count),
        signals: graph
            .nodes
            .iter()
            .map(|n| (n.id.clone(), Vec::with_capacity(count)))
            .collect(),
        solver: o.solver,
    };
    for sample in 0..count {
        let t = if sample + 1 == count {
            o.stop
        } else {
            (o.start + sample as f64 * o.step).min(o.stop)
        };
        let values = compiled.evaluate(t, &state)?;
        if !observer(Sample {
            time: t,
            nodes: &graph.nodes,
            values: &values,
        }) {
            return Err(Error::Cancelled);
        }
        trace.time.push(t);
        for (i, node) in graph.nodes.iter().enumerate() {
            trace.signals.get_mut(&node.id).unwrap().push(values[i]);
        }
        if sample + 1 == count {
            break;
        }
        let h = (o.stop - t).min(o.step);
        if t + h <= t {
            return Err(Error::Options(
                "step too small to advance floating-point time".into(),
            ));
        }
        let k1 = compiled.derivative(&values);
        let mut next = match o.solver {
            Solver::Euler => state
                .iter()
                .zip(&k1)
                .map(|(s, k)| s + h * k)
                .collect::<Vec<_>>(),
            Solver::Rk4 => {
                let stage = |k: &[f64], factor: f64| {
                    state
                        .iter()
                        .zip(k)
                        .map(|(s, k)| s + h * factor * k)
                        .collect::<Vec<_>>()
                };
                let k2 = compiled.derivative(&compiled.evaluate(t + h / 2.0, &stage(&k1, 0.5))?);
                let k3 = compiled.derivative(&compiled.evaluate(t + h / 2.0, &stage(&k2, 0.5))?);
                // Evaluate the left limit at the interval boundary: Step switches are sampled
                // at the next tick, not prematurely integrated over the preceding interval.
                let endpoint = t + h;
                let stage_time = if graph.nodes.iter().any(
                    |n| matches!(n.kind,Kind::Step {time,..} if (time-endpoint).abs()<=1e-9*o.step),
                ) {
                    endpoint - (1e-9 * o.step).max(32.0 * f64::EPSILON * endpoint.abs().max(1.0))
                } else {
                    endpoint
                };
                let k4 = compiled.derivative(&compiled.evaluate(stage_time, &stage(&k3, 1.0))?);
                (0..state.len())
                    .map(|i| state[i] + h * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]) / 6.0)
                    .collect()
            }
        };
        for (s, &i) in compiled.states.iter().enumerate() {
            if matches!(graph.nodes[i].kind, Kind::UnitDelay { .. }) {
                next[s] = values[compiled.inputs[i][0]];
            }
            if !next[s].is_finite() {
                return Err(block_error(
                    &graph.nodes[i].id,
                    format!("non-finite state at time {}", t + h),
                ));
            }
        }
        state = next;
    }
    Ok(trace)
}

mod flatten;
mod import;
pub use import::{compile, simulate_model, simulate_model_with_observer};
