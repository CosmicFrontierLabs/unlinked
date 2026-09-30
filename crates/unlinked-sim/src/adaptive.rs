//! Dormand–Prince embedded 5(4) integration with a fixed observation grid.
//!
//! Tableau: Dormand and Prince, "A family of embedded Runge-Kutta formulae",
//! Journal of Computational and Applied Mathematics 6 (1980), 19–26.
//! We use the fifth-order solution, an embedded fourth-order error estimate,
//! and the maximum normalized component error. This is not MATLAB ode45's
//! dense-output/event implementation; observation boundaries are exact stops.
use super::{Compiled, Error, Kind, Options};

const TABLEAU: &[(f64, &[f64])] = &[
    (1.0 / 5.0, &[1.0 / 5.0]),
    (3.0 / 10.0, &[3.0 / 40.0, 9.0 / 40.0]),
    (4.0 / 5.0, &[44.0 / 45.0, -56.0 / 15.0, 32.0 / 9.0]),
    (
        8.0 / 9.0,
        &[
            19372.0 / 6561.0,
            -25360.0 / 2187.0,
            64448.0 / 6561.0,
            -212.0 / 729.0,
        ],
    ),
    (
        1.0,
        &[
            9017.0 / 3168.0,
            -355.0 / 33.0,
            46732.0 / 5247.0,
            49.0 / 176.0,
            -5103.0 / 18656.0,
        ],
    ),
    (
        1.0,
        &[
            35.0 / 384.0,
            0.0,
            500.0 / 1113.0,
            125.0 / 192.0,
            -2187.0 / 6784.0,
            11.0 / 84.0,
        ],
    ),
];

pub(super) struct Adaptive {
    next_step: f64,
    attempts: usize,
}
impl Adaptive {
    pub(super) fn new(step: f64) -> Self {
        Self {
            next_step: step,
            attempts: 0,
        }
    }

    pub(super) fn advance(
        &mut self,
        compiled: &Compiled<'_>,
        options: &Options,
        start: f64,
        end: f64,
        initial: &[f64],
    ) -> Result<Vec<f64>, Error> {
        let mut state = initial.to_vec();
        let mut time = start;
        let mut rejected = false;
        while time < end {
            if self.attempts >= options.max_internal_steps {
                return Err(Error::Options(
                    "adaptive integration step budget exceeded".into(),
                ));
            }
            self.attempts += 1;
            let proposed = self.next_step;
            let h = proposed.min(end - time);
            let next_time = if h == end - time { end } else { time + h };
            if !h.is_finite() || h <= 0.0 || next_time <= time {
                return Err(Error::Options(
                    "adaptive step too small to advance floating-point time".into(),
                ));
            }
            // Recompute the first derivative after each interval. In particular,
            // an endpoint derivative from the left side of a Step must never be
            // reused as the derivative on its right side (no FSAL across events).
            let mut stages = vec![compiled.derivative(&compiled.evaluate(time, &state, false)?)];
            let mut candidate = Vec::new();
            for &(fraction, weights) in TABLEAU {
                candidate = state
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        value
                            + h * weights
                                .iter()
                                .zip(&stages)
                                .map(|(weight, derivative)| weight * derivative[i])
                                .sum::<f64>()
                    })
                    .collect();
                if candidate.iter().any(|value| !value.is_finite()) {
                    return Err(Error::Options(
                        "non-finite adaptive stage; reduce output step or check model dynamics"
                            .into(),
                    ));
                }
                let stage_time = if fraction == 1.0 {
                    next_time
                } else {
                    time + fraction * h
                };
                stages.push(compiled.derivative(&compiled.evaluate(
                    stage_time,
                    &candidate,
                    fraction == 1.0,
                )?));
            }
            let error_weights = [
                71.0 / 57600.0,
                0.0,
                -71.0 / 16695.0,
                71.0 / 1920.0,
                -17253.0 / 339200.0,
                22.0 / 525.0,
                -1.0 / 40.0,
            ];
            let mut error: f64 = 0.0;
            for (i, &node) in compiled.states.iter().enumerate() {
                if !matches!(compiled.graph.nodes[node].kind, Kind::Integrator { .. }) {
                    continue;
                }
                let estimate = h * error_weights
                    .iter()
                    .zip(&stages)
                    .map(|(weight, derivative)| weight * derivative[i])
                    .sum::<f64>();
                let scale = options.absolute_tolerance
                    + options.relative_tolerance * state[i].abs().max(candidate[i].abs());
                let ratio = estimate.abs() / scale;
                if !ratio.is_finite() {
                    return Err(Error::Options("non-finite adaptive error estimate".into()));
                }
                error = error.max(ratio);
            }
            let factor = if error == 0.0 {
                5.0
            } else {
                (0.9 * error.powf(-0.2)).clamp(0.2, 5.0)
            };
            if error <= 1.0 {
                state = candidate;
                time = next_time;
                self.next_step = h * if rejected { factor.min(1.0) } else { factor };
                if h < proposed && !rejected {
                    // An observation boundary is not evidence that the natural
                    // integration step must shrink in the next interval.
                    self.next_step = self.next_step.max(proposed);
                }
                rejected = false;
            } else {
                self.next_step = h * factor.min(1.0);
                rejected = true;
            }
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Graph, Node, Solver, Wire};
    #[test]
    fn clipping_to_observation_boundary_does_not_force_regrowth() {
        let graph = Graph {
            nodes: vec![
                Node {
                    id: "constant".into(),
                    name: "constant".into(),
                    kind: Kind::Constant { value: 1.0 },
                },
                Node {
                    id: "state".into(),
                    name: "state".into(),
                    kind: Kind::Integrator { initial: 0.0 },
                },
            ],
            wires: vec![Wire {
                source: "constant".into(),
                target: "state".into(),
                input: 0,
            }],
        };
        let compiled = Compiled::new(&graph).unwrap();
        let options = Options {
            solver: Solver::Rk45,
            max_internal_steps: 2,
            ..Options::default()
        };
        let mut integrator = Adaptive::new(1.0);
        let state = integrator
            .advance(&compiled, &options, 0.0, 0.01, &[0.0])
            .unwrap();
        let state = integrator
            .advance(&compiled, &options, 0.01, 1.01, &state)
            .unwrap();
        assert!((state[0] - 1.01).abs() < 1e-12);
    }
}
