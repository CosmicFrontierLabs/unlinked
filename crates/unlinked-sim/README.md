# unlinked-sim

A deterministic, bounded simulator with scalar execution and array signal lowering written entirely in Rust, with no
filesystem, network, subprocess or native runtime dependency. `compile` resolves fixed signal dimensions and lowers
`unlinked_model::Model` to a scalar `Graph`; `simulate` runs an explicit graph;
`simulate_model` combines both. Trace JSON contains sample times, stable block
IDs mapped to scalar sample vectors, and the selected solver.

Implemented: Constant/Ground, Clock, Step, time-based Sine, Gain, Bias, Sum/Add,
Product/division, Saturation, Integrator, UnitDelay, Abs, scalar trigonometric
and math operations, scalar relational comparisons, logical operators, nonzero
Switch routing, and single-input sinks. Logic and relational outputs use exact
0/1 scalar values, including declared boolean outputs. Switch currently accepts
only `Criteria=u2 ~= 0`; threshold criteria require signal datatype propagation
to reproduce boolean-control behavior and therefore reject. Explicit zero-crossing
detection on Switch/RelationalOperator rejects; no event root finding is implied. Ordinary virtual subsystems lower
to identity boundary nodes with qualified block IDs. Raw block parameters and model
workspace expressions use the pure `unlinked-matlab::eval_array_expr` evaluator. Workspace dependencies
resolve iteratively; unresolved/cyclic references fail.

Finite real scalar, vector, and matrix Constant signals are supported. Gain,
Sum/Product with multiple inputs, comparisons, and ordinary elementwise blocks
support scalar expansion or matching shapes. Matrix `K*u` gains lower to weighted
sums; right matrix multiplication and general matrix Product reject. Mux accepts
scalar/vector inputs and concatenates them; Demux supports positive explicit
widths or equal-width output counts. A non-divisible equal split rejects rather
than guessing widths. Single-input Sum/Product/Logic reduce a 1-D vector;
axis-specific and matrix reductions reject. Vector/matrix Integrator and
UnitDelay states lower componentwise, with scalar initial-condition expansion.
One-dimensional vector signals have no row/column orientation. Matrix signals
remain two-dimensional and are never implicitly flattened by Mux/Demux.

Scalar trace IDs remain unchanged. Vectors use `id[index]`, matrices use
`id[row,column]`, and multi-output Demux blocks use `id:out:port[index]`, with
1-based indices and column-major storage. A collision with an existing original
ID receives a deterministic `#number` suffix. Generated matrix-product terms
are also available as named trace signals. Shape propagation rejects conflicting
or unresolved dimensions and has a ten-million-work-item budget. Limits are
1,024 elements per signal/array parameter, 100,000 lowered nodes, one million
wires, and 100,000 total workspace elements; large expansions reject before
allocating scalar node IDs.

Euler, classical fourth-order Runge–Kutta, and adaptive Dormand–Prince 5(4)
(`Solver::Rk45`, JSON `rk45`) advance continuous states simultaneously. Rk45
uses internal accepted/rejected substeps while preserving the requested output
grid. Its maximum component error is scaled by `absolute_tolerance +
relative_tolerance * max(abs(old), abs(new))`, defaulting to 1e-9 and 1e-6.
These are local error targets, not a global-error guarantee. It stops at output
boundaries rather than interpolating dense output. Defaults allow 100,000 total
internal attempts (accepted plus rejected), with a hard cap of 1,000,000. Discrete states and held signals remain fixed during continuous solver stages.
The initial sample is recorded, and the final step may be shorter to reach the
requested stop time. Step discontinuities must align with the sampling grid.
The solver uses the left limit at a transition when integrating the preceding
interval. Time is in seconds, sine frequency is radians/second.

Proper SISO TransferFcn blocks of order zero through 64 lower to zero-initial-state
controllable canonical equations. Numerator and denominator accept finite numeric
row expressions, including array workspace variables. Improper transfer functions,
nonzero continuous transfer initial conditions, and per-block absolute tolerances
reject. No pole/zero cancellation occurs; poorly conditioned high-order polynomials
may require a smaller step or a different model realization.

SISO StateSpace supports finite constant A/B/C/D matrices with one through 64
states, scalar/vector InitialCondition (or X0), and global solver tolerances.
A is N-by-N, B is N-by-1, C is 1-by-N, and D is scalar; all default to 1.
Initial states default to zero. Dynamic D tuning and MIMO dimensions reject.

DiscreteTransferFcn supports proper descending-power z polynomials through order
64 with direct-form-II states, including scalar/vector InitialStates. Raw denominator
coefficients preserve the meaning of initial delay states. Coefficients must come
from dialog expressions; external resets, frame processing, and fixed-point arithmetic reject.
Direct outputs use an explicit sample-and-hold node. A direct-feedthrough discrete
output reaching a continuous Integrator without an intervening UnitDelay still
rejects conservatively pending broader mixed-rate validation; this includes constant
sources. Strictly proper discrete outputs can drive continuous states.

UnitDelay, DiscreteTransferFcn, and DigitalClock support independent positive sample
periods that are integer multiples of the output step, capped at one million ticks.
Inherited delays/transfer blocks use the output step in single-rate models.
Multirate models require explicit sample times on every discrete state block;
full inherited-rate propagation is unsupported. DigitalClock defaults to one
second, following the [block reference](https://www.mathworks.com/help/simulink/slref/digitalclock.html).
Scheduling uses integer ticks: each delay initially outputs its initial condition,
samples its input at a hit, and publishes that sampled value at the next hit.
Simultaneous hits commit all pending delay states before sampling any new inputs.
Outputs hold between hits, including through every RK stage. Start time must align
with every discrete block's zero-phase period, and stop time with the output grid.
Noncommensurate periods, nonzero sample offsets, and general rate-transition/task
priority semantics are unsupported.

Generated internal states appear in the graph and trace with named paths; the
original block ID remains its output signal. Dynamic matrix coefficients permit
up to 4,096 entries for a 64-by-64 A matrix.

Options explicitly override imported solver configuration. This is a supported
subset, not a claim of general Simulink numerical equivalence. It rejects
algebraic loops, missing/multiple drivers, non-finite signals, unknown block
types, atomic/conditional subsystems, masks, library links, integer types,
external resets and unsupported sample-rate configurations. Root Inports require explicit constant
bindings through `evaluate_inputs` and `compile_with_inputs` or
`simulate_model_with_inputs`. Bindings use original root block IDs, resolve model
workspace expressions, validate declared dimensions and preserve datatype
restrictions; unbound inputs never silently become zero. Workspace names overriding built-in constants reject to avoid
ambiguous dependency ordering. Scope and ToWorkspace values appear in the
trace; these blocks do not produce external files.

Output is limited to 1,000,001 samples and ten million scalar values. Default
options cap samples at 100,001. Integration accuracy must be checked for the
model and chosen step; there is no event root finder, stiff solver, implicit solver or Stateflow
execution yet. Rk45 implements the Dormand–Prince pair, not full MATLAB ode45
compatibility. Cancellation observers run at output boundaries; internal work
between boundaries is bounded by the attempt limit.

Tests compare feedback decay against `exp(-t)` and the closed-form Euler
recurrence, check simultaneous delay updates and decimal step boundaries, and
exercise importer diagnostics, resource budgets and invalid graphs.

Pure `MatlabFunction` execution nodes support up to 256 scalar inputs and exactly
one finite numeric/logical scalar output. The engine prepares each function once
per graph execution and shares one `ArrayBudget` across all function nodes,
observation samples and continuous solver stages. No subprocess or native
compiler runs during simulation. `simulate_with_observer_and_budget` accepts a
budget carrying a cooperative deadline/cancellation callback. Source totals are
limited to 1 MiB and 1,024 functions per graph; individual interpreter limits also
apply. Persistent/global state, printing, external access, non-scalar outputs and
multiple output ports reject explicitly.
