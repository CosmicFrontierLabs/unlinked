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
    /// Adaptive Dormand–Prince 5(4), sampled on the requested output grid.
    Rk45,
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
    /// Relative local error tolerance for Rk45 (positive, at most one).
    pub relative_tolerance: f64,
    /// Absolute local error tolerance for Rk45 (positive and finite).
    pub absolute_tolerance: f64,
    /// Total accepted and rejected Rk45 attempts across the complete run.
    pub max_internal_steps: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            start: 0.0,
            stop: 10.0,
            step: 0.01,
            solver: Solver::Rk4,
            max_samples: 100_001,
            relative_tolerance: 1e-6,
            absolute_tolerance: 1e-9,
            max_internal_steps: 100_000,
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
    /// Time-based Pulse Generator: zero before `phase_delay`, then `amplitude`
    /// for the first `width` seconds of every `period`. Edges follow the
    /// parameter-effects figure of the
    /// [block reference](https://www.mathworks.com/help/simulink/slref/pulsegenerator.html):
    /// the rising edge at `phase_delay + k*period` takes the high value and
    /// the falling edge `width` later the low one, like a Step.
    PulseGenerator {
        amplitude: f64,
        period: f64,
        /// `period * PulseWidth / 100`, in seconds.
        width: f64,
        phase_delay: f64,
    },
    /// Signal Generator driven by simulation time; `frequency` is in hertz
    /// when `hertz`, else radians per second. Shapes follow the default-output
    /// figures of the
    /// [block reference](https://www.mathworks.com/help/simulink/slref/signalgenerator.html):
    /// `sine` is `A sin(ωt)`, `square` is `-A` over the first half of each
    /// period and `+A` over the second, and `sawtooth` falls linearly from
    /// `+A` at the start of each period to `-A` at its end.
    SignalGenerator {
        waveform: String,
        amplitude: f64,
        frequency: f64,
        hertz: bool,
    },
    /// Ramp: `initial` until `start`, then `initial + slope * (t - start)`
    /// ([block reference](https://www.mathworks.com/help/simulink/slref/ramp.html)).
    Ramp {
        slope: f64,
        start: f64,
        initial: f64,
    },
    /// Chirp Signal: `sin(2π(f1 t + (f2 - f1) t² / (2T)))`, the sine whose
    /// instantaneous frequency sweeps linearly from `f1` at time zero to
    /// `f2` at `target_time` and keeps changing at that rate afterwards. The
    /// block has no phase parameter
    /// ([block reference](https://www.mathworks.com/help/simulink/slref/chirpsignal.html)).
    Chirp {
        initial_frequency: f64,
        target_time: f64,
        target_frequency: f64,
    },
    /// Repeating Sequence: linear interpolation of `values` over `times`
    /// (strictly increasing from zero), repeated with period `times.last()`
    /// ([block reference](https://www.mathworks.com/help/simulink/slref/repeatingsequence.html)).
    RepeatingSequence {
        times: Vec<f64>,
        values: Vec<f64>,
    },
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
    /// One tick per requested simulation step.
    UnitDelay {
        initial: f64,
    },
    /// Delay sampled at positive integer multiples of the observation step.
    RateDelay {
        initial: f64,
        period_ticks: usize,
    },
    /// Simulation time sampled at positive integer multiples of the step.
    DigitalClock {
        period_ticks: usize,
    },
    /// Direct-feedthrough sampling at hits, held during continuous solver stages.
    SampleHold {
        period_ticks: usize,
    },
    /// Seeded Gaussian samples held between discrete hits. Sequence is Unlinked-specific.
    RandomNumber {
        mean: f64,
        variance: f64,
        seed: u32,
        period_ticks: usize,
    },
    /// Seeded uniform samples held between discrete hits. Sequence is Unlinked-specific.
    UniformRandomNumber {
        minimum: f64,
        maximum: f64,
        seed: u32,
        period_ticks: usize,
    },
    /// Scalar numeric switch. All three inputs are evaluated; no lazy branch execution.
    Switch,
    Relational {
        operation: String,
    },
    Logic {
        operation: String,
        inputs: usize,
    },
    Abs,
    /// One-input elementwise function; see [`UNARY_OPERATIONS`].
    Unary {
        operation: String,
    },
    /// Two-input elementwise function; see [`BINARY_OPERATIONS`].
    Binary {
        operation: String,
    },
    /// Polynomial in descending powers, evaluated by Horner's rule.
    Polyval {
        coefficients: Vec<f64>,
    },
    /// Elementwise minimum or maximum over `inputs` scalar inputs.
    MinMax {
        maximum: bool,
        inputs: usize,
    },
    /// Pure, stateless function with scalar inputs and exactly one scalar output.
    MatlabFunction {
        script: String,
        inputs: usize,
    },
    Sink,
}
impl Kind {
    fn input_count(&self) -> usize {
        match self {
            Self::Constant { .. }
            | Self::Step { .. }
            | Self::Sine { .. }
            | Self::Clock
            | Self::PulseGenerator { .. }
            | Self::SignalGenerator { .. }
            | Self::Ramp { .. }
            | Self::Chirp { .. }
            | Self::RepeatingSequence { .. }
            | Self::DigitalClock { .. }
            | Self::RandomNumber { .. }
            | Self::UniformRandomNumber { .. } => 0,
            Self::Switch => 3,
            Self::Relational { .. } | Self::Binary { .. } => 2,
            Self::Logic { inputs, .. }
            | Self::MatlabFunction { inputs, .. }
            | Self::MinMax { inputs, .. } => *inputs,
            Self::Sum { signs } => signs.len(),
            Self::Product { divide } => divide.len(),
            _ => 1,
        }
    }
    fn is_state(&self) -> bool {
        matches!(
            self,
            Self::Integrator { .. }
                | Self::UnitDelay { .. }
                | Self::RateDelay { .. }
                | Self::DigitalClock { .. }
                | Self::SampleHold { .. }
                | Self::RandomNumber { .. }
                | Self::UniformRandomNumber { .. }
        )
    }
    fn direct_feedthrough(&self) -> bool {
        !self.is_state() || matches!(self, Self::SampleHold { .. })
    }
    fn period(&self) -> Option<usize> {
        match self {
            Self::UnitDelay { .. } => Some(1),
            Self::RateDelay { period_ticks, .. }
            | Self::DigitalClock { period_ticks }
            | Self::SampleHold { period_ticks }
            | Self::RandomNumber { period_ticks, .. }
            | Self::UniformRandomNumber { period_ticks, .. } => Some(*period_ticks),
            _ => None,
        }
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

/// Scalar one-input operations accepted by [`Kind::Unary`]. Trigonometric and
/// hyperbolic names follow the MathWorks
/// [Trigonometric Function](https://www.mathworks.com/help/simulink/slref/trigonometricfunction.html)
/// reference, the remaining names the
/// [Math Function](https://www.mathworks.com/help/simulink/slref/mathfunction.html),
/// [Sqrt](https://www.mathworks.com/help/simulink/slref/sqrt.html) and
/// [Rounding Function](https://www.mathworks.com/help/simulink/slref/roundingfunction.html)
/// references. `sign` is the Signum block, `negate` the Unary Minus block, and
/// `transpose`/`hermitian` are the per-element identities left after the shape
/// permutation performed during lowering. `conj` is the identity on real
/// signals. Results that leave the reals (sqrt or log of a negative, acosh
/// below one, reciprocal of zero) surface as the engine's non-finite signal
/// error; nothing is substituted.
pub const UNARY_OPERATIONS: &[&str] = &[
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "sinh",
    "cosh",
    "tanh",
    "asinh",
    "acosh",
    "atanh",
    "exp",
    "log",
    "2^u",
    "10^u",
    "log10",
    "magnitude^2",
    "square",
    "conj",
    "reciprocal",
    "transpose",
    "hermitian",
    "sqrt",
    "signedSqrt",
    "rSqrt",
    "sign",
    "floor",
    "ceil",
    "round",
    "fix",
    "negate",
];
/// Scalar two-input operations accepted by [`Kind::Binary`]. `atan2(y, x)`
/// takes the y component on the first port as in the Trigonometric Function
/// reference. `signedPow` is the Math Function `pow` with *Signed power* on
/// and a real output type, `sign(u)*|u|^v`; plain `pow` is `u^v`. `rem` and
/// `mod` use MATLAB's conventions: `rem(x, y) = x - fix(x/y)*y` takes the sign
/// of `x` and `rem(x, 0)` is NaN (a non-finite signal error here);
/// `mod(x, y) = x - floor(x/y)*y` takes the sign of `y` and `mod(x, 0) = x`
/// ([mod](https://www.mathworks.com/help/matlab/ref/double.mod.html),
/// [rem](https://www.mathworks.com/help/matlab/ref/double.rem.html)).
pub const BINARY_OPERATIONS: &[&str] = &["atan2", "pow", "signedPow", "hypot", "rem", "mod"];

fn unary(operation: &str, u: f64) -> f64 {
    match operation {
        "sin" => u.sin(),
        "cos" => u.cos(),
        "tan" => u.tan(),
        "asin" => u.asin(),
        "acos" => u.acos(),
        "atan" => u.atan(),
        "sinh" => u.sinh(),
        "cosh" => u.cosh(),
        "tanh" => u.tanh(),
        "asinh" => u.asinh(),
        "acosh" => u.acosh(),
        "atanh" => u.atanh(),
        "exp" => u.exp(),
        "log" => u.ln(),
        "2^u" => 2f64.powf(u),
        "10^u" => 10f64.powf(u),
        "log10" => u.log10(),
        "magnitude^2" | "square" => u * u,
        "conj" | "transpose" | "hermitian" => u,
        "reciprocal" => 1.0 / u,
        "sqrt" => u.sqrt(),
        "signedSqrt" => sign(u) * u.abs().sqrt(),
        "rSqrt" => 1.0 / u.sqrt(),
        "sign" => sign(u),
        "floor" => u.floor(),
        "ceil" => u.ceil(),
        // MATLAB rounds halves away from zero, as `f64::round` does.
        "round" => u.round(),
        "fix" => u.trunc(),
        "negate" => -u,
        _ => unreachable!(),
    }
}
fn binary(operation: &str, u: f64, v: f64) -> f64 {
    match operation {
        "atan2" => u.atan2(v),
        "pow" => u.powf(v),
        // The Math Function reference defines Signed power as sign(u)*|u|^v
        // for every exponent (so (-2)^2 is -4, unlike plain pow).
        "signedPow" => sign(u) * u.abs().powf(v),
        "hypot" => u.hypot(v),
        // MATLAB rem/mod compensate for floating-point round-off so that
        // mod(4*pi, 2*pi) is exactly 0: with a non-integer divisor, a quotient
        // within eps of an integer yields 0 instead of a residual (Simulink's
        // rt_remd_snf/rt_modd_snf do the same).
        "rem" => {
            if v != 0.0 && v != v.trunc() && quotient_is_integer(u, v) {
                0.0
            } else {
                u % v
            }
        }
        "mod" => {
            if v == 0.0 {
                u
            } else {
                let r = u % v;
                if r == 0.0 || (v != v.trunc() && quotient_is_integer(u, v)) {
                    0.0
                } else if (r < 0.0) != (v < 0.0) {
                    r + v
                } else {
                    r
                }
            }
        }
        _ => unreachable!(),
    }
}
/// Whether `u / v` is an integer up to floating-point round-off, the test
/// MATLAB's `rem`/`mod` use to produce exact zeros for non-integer divisors.
fn quotient_is_integer(u: f64, v: f64) -> bool {
    let q = (u / v).abs();
    q.is_finite() && (q - q.round()).abs() <= f64::EPSILON * q
}
/// MATLAB `sign`: zero for zero, unlike `f64::signum`.
fn sign(u: f64) -> f64 {
    if u == 0.0 {
        0.0
    } else {
        u.signum()
    }
}

struct Compiled<'a> {
    graph: &'a Graph,
    inputs: Vec<Vec<usize>>,
    order: Vec<usize>,
    states: Vec<usize>,
    matlab: matlab_function::Runtime,
}
fn on_grid(ticks: f64) -> bool {
    ticks == 0.0 || (ticks.round() >= 1.0 && (ticks - ticks.round()).abs() <= 1e-9)
}

fn block_error(id: &str, message: impl Into<String>) -> Error {
    Error::Block {
        block: id.to_owned(),
        message: message.into(),
    }
}

/// Whether `x` and `edge` differ only by rounding error at magnitude `scale`.
/// Periodic edges are sums of grid-aligned spans, so a few ulps are allowed.
fn near_edge(x: f64, edge: f64, scale: f64) -> bool {
    let magnitude = scale
        .abs()
        .max(x.abs())
        .max(edge.abs())
        .max(f64::MIN_POSITIVE);
    (x - edge).abs() <= 64.0 * f64::EPSILON * magnitude
}
/// Whether `x` lies before a transition at `edge`. At the transition itself
/// the right-hand value applies unless `left_limit` asks for the value just
/// before it, the convention Step uses when integrating the preceding interval.
fn before_edge(x: f64, edge: f64, scale: f64, left_limit: bool) -> bool {
    let near = near_edge(x, edge, scale);
    (x < edge && !near) || (left_limit && near)
}
/// Offset of `t` into its cycle of `period` counted from `origin`, in
/// `[0, period]`: exactly zero at a wrap and exactly `period` for the left
/// limit there, so periodic waveforms treat wraps like Step transitions.
fn cycle_offset(t: f64, origin: f64, period: f64, left_limit: bool) -> f64 {
    let mut start = origin + ((t - origin) / period).floor() * period;
    if !before_edge(t, start + period, t, left_limit) {
        start += period;
    } else if before_edge(t, start, t, left_limit) {
        start -= period;
    }
    let offset = t - start;
    if near_edge(offset, period, t) {
        period
    } else if near_edge(offset, 0.0, t) {
        0.0
    } else {
        offset
    }
}
/// Whether a pulse train high for `width` seconds from `phase + k*period`
/// is high at `t`; zero before `phase`.
fn pulse_high(t: f64, phase: f64, width: f64, period: f64, left_limit: bool) -> bool {
    if before_edge(t, phase, t, left_limit) {
        return false;
    }
    before_edge(
        cycle_offset(t, phase, period, left_limit),
        width,
        t,
        left_limit,
    )
}
/// Piecewise-linear interpolation of `values` over strictly increasing
/// `times`; `u` is clamped to the table's range.
fn interpolate(times: &[f64], values: &[f64], u: f64) -> f64 {
    let i = times.partition_point(|&x| x <= u).clamp(1, times.len() - 1);
    let (t0, t1) = (times[i - 1], times[i]);
    let a = ((u - t0) / (t1 - t0)).clamp(0.0, 1.0);
    values[i - 1] + a * (values[i] - values[i - 1])
}
/// Period of a Signal Generator waveform in seconds (also maps a period back
/// to a frequency, the relation being its own inverse).
fn generator_period(frequency: f64, hertz: bool) -> f64 {
    if hertz {
        1.0 / frequency
    } else {
        std::f64::consts::TAU / frequency
    }
}

/// Timing constraints of time-driven sources, checked both when compiling a
/// model and when executing a graph. Discontinuities must fall on sample
/// times so that no solver stage straddles a jump: this is the fixed-step
/// requirement the Pulse Generator reference documents, applied to every
/// source with edges. Smooth sources (sine, ramp, chirp) are unconstrained,
/// except that the Chirp Signal reference requires a zero start time.
pub(crate) fn source_timing(kind: &Kind, o: &Options) -> Result<(), &'static str> {
    let aligned = |time: f64| on_grid(((time - o.start) / o.step).abs());
    let spans = |span: f64| on_grid(span / o.step);
    match kind {
        Kind::Step { time, .. } if *time > o.start && *time < o.stop && !aligned(*time) => {
            Err("Step transition must align with fixed-step grid")
        }
        // A non-positive Period is rejected as such by `Compiled::new`, not
        // as a misaligned edge.
        Kind::PulseGenerator {
            period,
            width,
            phase_delay,
            ..
        } if *period > 0.0 && !(aligned(*phase_delay) && spans(*period) && spans(*width)) => {
            Err("Pulse Generator edges must align with the fixed-step grid: PhaseDelay must fall on a sample time, and Period and Period*PulseWidth/100 must be whole numbers of steps")
        }
        // Non-positive frequencies are rejected by parameter validation.
        Kind::SignalGenerator {
            waveform,
            frequency,
            hertz,
            ..
        } if waveform != "sine" && *frequency > 0.0 => {
            let period = generator_period(*frequency, *hertz);
            let edge = if waveform == "square" {
                period / 2.0
            } else {
                period
            };
            if aligned(0.0) && spans(edge) {
                Ok(())
            } else {
                Err("square and sawtooth Signal Generator edges must align with the fixed-step grid: time zero must be a sample time and the half period (square) or period (sawtooth) a whole number of steps; choose Hertz units or a step dividing the period")
            }
        }
        Kind::Chirp { .. } if o.start != 0.0 => {
            Err("Chirp Signal requires the simulation start time to be 0")
        }
        Kind::RepeatingSequence { times, values }
            if values.first() != values.last()
                && !(aligned(0.0) && times.last().is_some_and(|p| spans(*p))) =>
        {
            Err("Repeating Sequence wraps discontinuously (first and last rep_seq_y differ), so its wraps must align with the fixed-step grid: time zero must be a sample time and the period a whole number of steps")
        }
        _ => Ok(()),
    }
}
/// Snap accepted edge times onto the sample grid so transition and sample
/// times are computed identically; see `source_timing`.
fn snap_edges(kind: &mut Kind, o: &Options) {
    let time = |t: f64| o.start + ((t - o.start) / o.step).round() * o.step;
    let span = |d: f64| (d / o.step).round() * o.step;
    match kind {
        Kind::Step { time: t, .. } => {
            if *t >= o.start && *t <= o.stop && on_grid((*t - o.start) / o.step) {
                *t = time(*t);
            }
        }
        Kind::PulseGenerator {
            period,
            width,
            phase_delay,
            ..
        } => {
            *phase_delay = time(*phase_delay);
            *period = span(*period);
            *width = span(*width);
        }
        Kind::SignalGenerator {
            waveform,
            frequency,
            hertz,
            ..
        } if waveform != "sine" => {
            let period = span(generator_period(*frequency, *hertz));
            *frequency = generator_period(period, *hertz);
        }
        Kind::RepeatingSequence { times, values } if values.first() != values.last() => {
            if let Some(last) = times.last_mut() {
                *last = span(*last);
            }
        }
        _ => {}
    }
}
impl<'a> Compiled<'a> {
    fn new(graph: &'a Graph) -> Result<Self, Error> {
        let matlab = matlab_function::Runtime::prepare(graph)?;
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
                Kind::RandomNumber { mean, variance, .. } => {
                    if *variance < 0.0 {
                        return Err(block_error(&node.id, "Variance must be nonnegative"));
                    }
                    vec![*mean, *variance]
                }
                Kind::UniformRandomNumber {
                    minimum, maximum, ..
                } => {
                    if minimum >= maximum {
                        return Err(block_error(&node.id, "Minimum must be less than Maximum"));
                    }
                    vec![*minimum, *maximum]
                }
                Kind::Integrator { initial }
                | Kind::UnitDelay { initial }
                | Kind::RateDelay { initial, .. } => vec![*initial],
                Kind::PulseGenerator {
                    amplitude,
                    period,
                    width,
                    phase_delay,
                } => {
                    if *period <= 0.0 {
                        return Err(block_error(&node.id, "Period must be positive"));
                    }
                    if *width <= 0.0 || *width >= *period {
                        return Err(block_error(&node.id, "Period*PulseWidth/100 must lie strictly between 0 and Period: Simulink rejects a time-based Pulse Generator with constant output"));
                    }
                    if *phase_delay < 0.0 {
                        return Err(block_error(&node.id, "PhaseDelay must be nonnegative"));
                    }
                    vec![*amplitude, *period, *width, *phase_delay]
                }
                Kind::SignalGenerator {
                    waveform,
                    amplitude,
                    frequency,
                    ..
                } => {
                    if !["sine", "square", "sawtooth"].contains(&waveform.as_str()) {
                        return Err(block_error(
                            &node.id,
                            format!("unsupported Signal Generator WaveForm={waveform}"),
                        ));
                    }
                    if waveform != "sine" && *frequency <= 0.0 {
                        return Err(block_error(
                            &node.id,
                            "square and sawtooth waveforms require a positive Frequency",
                        ));
                    }
                    vec![*amplitude, *frequency]
                }
                Kind::Ramp {
                    slope,
                    start,
                    initial,
                } => vec![*slope, *start, *initial],
                Kind::Chirp {
                    initial_frequency,
                    target_time,
                    target_frequency,
                } => {
                    if *target_time <= 0.0 {
                        return Err(block_error(&node.id, "T (target time) must be positive"));
                    }
                    vec![*initial_frequency, *target_time, *target_frequency]
                }
                Kind::RepeatingSequence { times, values } => {
                    if times.len() < 2 || times.len() != values.len() || times.len() > 1024 {
                        return Err(block_error(&node.id, "rep_seq_t and rep_seq_y must be vectors of equal length, with 2 to 1024 points"));
                    }
                    if times[0] != 0.0 {
                        return Err(block_error(
                            &node.id,
                            "rep_seq_t must start at 0; a nonzero first time value is unsupported",
                        ));
                    }
                    if !times.windows(2).all(|w| w[1] > w[0]) {
                        return Err(block_error(
                            &node.id,
                            "rep_seq_t must be strictly increasing",
                        ));
                    }
                    times.iter().chain(values).copied().collect()
                }
                _ => vec![],
            };
            if node.kind.period().is_some_and(|p| p == 0 || p > 1_000_000) {
                return Err(block_error(
                    &node.id,
                    "sample period requires 1 to 1,000,000 ticks",
                ));
            }
            if values.iter().any(|v| !v.is_finite()) {
                return Err(block_error(&node.id, "non-finite parameter"));
            }
            if let Kind::Relational { operation } = &node.kind {
                if !["==", "~=", "<", "<=", ">", ">="].contains(&operation.as_str()) {
                    return Err(block_error(&node.id, "unsupported relational operator"));
                }
            }
            if let Kind::Logic { operation, inputs } = &node.kind {
                if !["AND", "OR", "NAND", "NOR", "XOR", "NXOR", "NOT"].contains(&operation.as_str())
                    || *inputs == 0
                    || *inputs > 1024
                    || (operation == "NOT" && *inputs != 1)
                {
                    return Err(block_error(
                        &node.id,
                        "unsupported logic operator or input count",
                    ));
                }
            }
            if let Kind::Unary { operation } = &node.kind {
                if !UNARY_OPERATIONS.contains(&operation.as_str()) {
                    return Err(block_error(
                        &node.id,
                        format!("unsupported unary operation {operation}"),
                    ));
                }
            }
            if let Kind::Binary { operation } = &node.kind {
                if !BINARY_OPERATIONS.contains(&operation.as_str()) {
                    return Err(block_error(
                        &node.id,
                        format!("unsupported binary operation {operation}"),
                    ));
                }
            }
            if let Kind::Polyval { coefficients } = &node.kind {
                if coefficients.is_empty()
                    || coefficients.len() > 1024
                    || coefficients.iter().any(|c| !c.is_finite())
                {
                    return Err(block_error(
                        &node.id,
                        "Polyval requires 1 to 1024 finite coefficients",
                    ));
                }
            }
            if let Kind::MinMax { inputs, .. } = &node.kind {
                if *inputs == 0 || *inputs > 1024 {
                    return Err(block_error(&node.id, "MinMax requires 1 to 1024 inputs"));
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
            if node.kind.direct_feedthrough() {
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
            if graph.nodes[i].kind.direct_feedthrough() {
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
            matlab,
        })
    }
    fn evaluate(&self, t: f64, state: &[f64], left_limit: bool) -> Result<Vec<f64>, Error> {
        self.evaluate_sample(t, state, left_limit, None)
    }
    fn evaluate_sample(
        &self,
        t: f64,
        state: &[f64],
        left_limit: bool,
        sample: Option<usize>,
    ) -> Result<Vec<f64>, Error> {
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
                    let near = (t - time).abs()
                        <= 8.0 * f64::EPSILON * time.abs().max(t.abs()).max(f64::MIN_POSITIVE);
                    if (t < *time && !near) || (left_limit && near) {
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
                Kind::PulseGenerator {
                    amplitude,
                    period,
                    width,
                    phase_delay,
                } => {
                    if pulse_high(t, *phase_delay, *width, *period, left_limit) {
                        *amplitude
                    } else {
                        0.0
                    }
                }
                Kind::SignalGenerator {
                    waveform,
                    amplitude,
                    frequency,
                    hertz,
                } => match waveform.as_str() {
                    "sine" => {
                        let angle = if *hertz {
                            std::f64::consts::TAU * frequency * t
                        } else {
                            frequency * t
                        };
                        amplitude * angle.sin()
                    }
                    "square" => {
                        let period = generator_period(*frequency, *hertz);
                        if pulse_high(t, period / 2.0, period / 2.0, period, left_limit) {
                            *amplitude
                        } else {
                            -amplitude
                        }
                    }
                    "sawtooth" => {
                        let period = generator_period(*frequency, *hertz);
                        amplitude * (1.0 - 2.0 * cycle_offset(t, 0.0, period, left_limit) / period)
                    }
                    _ => unreachable!(),
                },
                Kind::Ramp {
                    slope,
                    start,
                    initial,
                } => {
                    if t < *start {
                        *initial
                    } else {
                        initial + slope * (t - start)
                    }
                }
                Kind::Chirp {
                    initial_frequency,
                    target_time,
                    target_frequency,
                } => {
                    let sweep = (target_frequency - initial_frequency) / (2.0 * target_time);
                    (std::f64::consts::TAU * (initial_frequency * t + sweep * t * t)).sin()
                }
                Kind::RepeatingSequence { times, values } => {
                    let period = *times.last().unwrap();
                    interpolate(times, values, cycle_offset(t, 0.0, period, left_limit))
                }
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
                Kind::Switch => {
                    if x(1) != 0.0 {
                        x(0)
                    } else {
                        x(2)
                    }
                }
                Kind::Relational { operation } => f64::from(match operation.as_str() {
                    "==" => x(0) == x(1),
                    "~=" => x(0) != x(1),
                    "<" => x(0) < x(1),
                    "<=" => x(0) <= x(1),
                    ">" => x(0) > x(1),
                    ">=" => x(0) >= x(1),
                    _ => unreachable!(),
                }),
                Kind::Logic { operation, inputs } => {
                    let truth = |p| x(p) != 0.0;
                    f64::from(match operation.as_str() {
                        "AND" => (0..*inputs).all(truth),
                        "OR" => (0..*inputs).any(truth),
                        "NAND" => !(0..*inputs).all(truth),
                        "NOR" => !(0..*inputs).any(truth),
                        "XOR" => (0..*inputs).filter(|&p| truth(p)).count() % 2 == 1,
                        "NXOR" => (0..*inputs).filter(|&p| truth(p)).count() % 2 == 0,
                        "NOT" => !truth(0),
                        _ => unreachable!(),
                    })
                }
                Kind::Abs => x(0).abs(),
                Kind::Unary { operation } => unary(operation, x(0)),
                Kind::Binary { operation } => binary(operation, x(0), x(1)),
                Kind::Polyval { coefficients } => coefficients
                    .iter()
                    .fold(0.0_f64, |acc, c| acc.mul_add(x(0), *c)),
                Kind::MinMax { maximum, inputs } => {
                    let values = (0..*inputs).map(x);
                    if *maximum {
                        values.fold(f64::NEG_INFINITY, f64::max)
                    } else {
                        values.fold(f64::INFINITY, f64::min)
                    }
                }
                Kind::MatlabFunction { inputs, .. } => self.matlab.evaluate(
                    i,
                    &self.graph.nodes[i].id,
                    (0..*inputs).map(x).collect(),
                )?,
                Kind::Sink => x(0),
                Kind::SampleHold { period_ticks } => {
                    if sample.is_some_and(|tick| tick % period_ticks == 0) {
                        x(0)
                    } else {
                        values[i]
                    }
                }
                Kind::Integrator { .. }
                | Kind::UnitDelay { .. }
                | Kind::RateDelay { .. }
                | Kind::DigitalClock { .. }
                | Kind::RandomNumber { .. }
                | Kind::UniformRandomNumber { .. } => unreachable!(),
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

/// Execute a bounded scalar graph on a fixed observation grid. Integrators are simultaneous, delays update
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
    observer: impl FnMut(Sample<'_>) -> bool,
) -> Result<Trace, Error> {
    simulate_with_observer_and_budget(
        graph,
        options,
        unlinked_matlab::ArrayBudget::default(),
        observer,
    )
}

/// Validate explicit options without evaluating any model or advancing time.
pub fn sample_count(o: &Options) -> Result<usize, Error> {
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
    if o.solver == Solver::Rk45
        && (!o.relative_tolerance.is_finite()
            || o.relative_tolerance <= 0.0
            || o.relative_tolerance > 1.0
            || !o.absolute_tolerance.is_finite()
            || o.absolute_tolerance <= 0.0
            || o.max_internal_steps == 0
            || o.max_internal_steps > 1_000_000)
    {
        return Err(Error::Options("Rk45 requires 0 < relative_tolerance <= 1, positive finite absolute_tolerance, and 1..=1,000,000 internal steps".into()));
    }
    let ticks = (o.stop - o.start) / o.step;
    let near_integer = on_grid(ticks);
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
    Ok(intervals as usize + 1)
}

/// As `simulate_with_observer`, with one function-execution budget shared across
/// all blocks, samples and solver stages. The budget may carry a deadline check.
pub fn simulate_with_observer_and_budget(
    graph: &Graph,
    options: &Options,
    budget: unlinked_matlab::ArrayBudget,
    mut observer: impl FnMut(Sample<'_>) -> bool,
) -> Result<Trace, Error> {
    let o = options;
    let count = sample_count(o)?;
    if graph.nodes.len() > 100_000 || graph.wires.len() > 1_000_000 {
        return Err(Error::Options("graph budget exceeded".into()));
    }
    if graph.nodes.iter().any(|n| n.kind.period().is_some()) {
        let ticks = (o.stop - o.start) / o.step;
        if !on_grid(ticks) {
            return Err(Error::Options(
                "discrete blocks require stop time on the fixed-step grid".into(),
            ));
        }
    }
    if graph.nodes.len().saturating_mul(count) > 10_000_000 {
        return Err(Error::Options(
            "signal output budget exceeded (10 million values)".into(),
        ));
    }
    for node in &graph.nodes {
        if let Some(period) = node.kind.period() {
            if period == 0 || period > 1_000_000 || !on_grid(o.start / (period as f64 * o.step)) {
                return Err(block_error(&node.id, "discrete sample period must be bounded and simulation start must align with its zero-phase grid"));
            }
        }
        source_timing(&node.kind, o).map_err(|message| block_error(&node.id, message))?;
    }
    // Accepted decimal transition times are snapped to the same grid used for
    // samples; validation tolerance must not create inconsistent stage values.
    let mut normalized = graph.clone();
    for node in &mut normalized.nodes {
        snap_edges(&mut node.kind, o);
    }
    let graph = &normalized;
    let compiled = Compiled::new(graph)?;
    compiled.matlab.budget.replace(budget);
    let mut state: Vec<f64> = compiled
        .states
        .iter()
        .map(|&i| match graph.nodes[i].kind {
            Kind::Integrator { initial }
            | Kind::UnitDelay { initial }
            | Kind::RateDelay { initial, .. } => initial,
            Kind::DigitalClock { .. }
            | Kind::SampleHold { .. }
            | Kind::RandomNumber { .. }
            | Kind::UniformRandomNumber { .. } => 0.0,
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
    let mut random: Vec<Option<random::Generator>> = compiled
        .states
        .iter()
        .map(|&i| match graph.nodes[i].kind {
            Kind::RandomNumber { seed, .. } | Kind::UniformRandomNumber { seed, .. } => {
                Some(random::Generator::new(seed))
            }
            _ => None,
        })
        .collect();
    let mut pending = state.clone();
    let mut adaptive = adaptive::Adaptive::new(o.step);
    for sample in 0..count {
        let t = if sample + 1 == count {
            o.stop
        } else {
            (o.start + sample as f64 * o.step).min(o.stop)
        };
        for (slot, &node) in compiled.states.iter().enumerate() {
            match graph.nodes[node].kind {
                Kind::DigitalClock { period_ticks } if sample % period_ticks == 0 => {
                    state[slot] = t
                }
                Kind::RandomNumber {
                    mean,
                    variance,
                    period_ticks,
                    ..
                } if sample % period_ticks == 0 => {
                    state[slot] = mean + variance.sqrt() * random[slot].as_mut().unwrap().normal();
                }
                Kind::UniformRandomNumber {
                    minimum,
                    maximum,
                    period_ticks,
                    ..
                } if sample % period_ticks == 0 => {
                    let u = random[slot].as_mut().unwrap().uniform();
                    state[slot] = (1.0 - u) * minimum + u * maximum;
                }
                Kind::UnitDelay { .. } if sample > 0 => state[slot] = pending[slot],
                Kind::RateDelay { period_ticks, .. }
                    if sample > 0 && sample % period_ticks == 0 =>
                {
                    state[slot] = pending[slot]
                }
                _ => {}
            }
        }
        for (slot, &node) in compiled.states.iter().enumerate() {
            if !state[slot].is_finite() {
                return Err(block_error(
                    &graph.nodes[node].id,
                    format!("non-finite state at time {t}"),
                ));
            }
        }
        let values = compiled.evaluate_sample(t, &state, false, Some(sample))?;
        for (slot, &node) in compiled.states.iter().enumerate() {
            match graph.nodes[node].kind {
                Kind::SampleHold { .. } => state[slot] = values[node],
                Kind::UnitDelay { .. } => pending[slot] = values[compiled.inputs[node][0]],
                Kind::RateDelay { period_ticks, .. } if sample % period_ticks == 0 => {
                    pending[slot] = values[compiled.inputs[node][0]]
                }
                _ => {}
            }
        }
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
        let next = match o.solver {
            Solver::Rk45 => {
                let end = if sample + 2 == count {
                    o.stop
                } else {
                    (o.start + (sample + 1) as f64 * o.step).min(o.stop)
                };
                adaptive.advance(&compiled, o, t, end, &state)?
            }
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
                let k2 = compiled.derivative(&compiled.evaluate(
                    t + h / 2.0,
                    &stage(&k1, 0.5),
                    false,
                )?);
                let k3 = compiled.derivative(&compiled.evaluate(
                    t + h / 2.0,
                    &stage(&k2, 0.5),
                    false,
                )?);
                // Only discontinuous Step outputs use the left limit. Smooth
                // sources still evaluate at the exact stage time.
                let k4 = compiled.derivative(&compiled.evaluate(t + h, &stage(&k3, 1.0), true)?);
                (0..state.len())
                    .map(|i| state[i] + h * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]) / 6.0)
                    .collect()
            }
        };
        for (s, &i) in compiled.states.iter().enumerate() {
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

mod adaptive;
mod chart_lowering;
pub mod commented;
pub mod diagnose;
mod flatten;
mod import;
mod inputs;
mod library;
mod matlab_function;
mod random;
pub use inputs::{
    compile_with_inputs, evaluate_inputs, evaluate_inputs_with_budget, simulate_model_with_inputs,
    InputValues,
};
mod state_space;
mod transfer;
mod vector;
pub use import::{compile, simulate_model, simulate_model_with_observer};
