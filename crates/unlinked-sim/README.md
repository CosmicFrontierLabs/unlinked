# unlinked-sim

A deterministic, bounded scalar simulator written entirely in Rust, with no
filesystem, network, subprocess or native runtime dependency. `compile` lowers
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
workspace expressions use `unlinked-matlab::eval_expr`. Workspace dependencies
resolve iteratively; unresolved/cyclic references fail.

Euler, classical fourth-order Runge–Kutta, and adaptive Dormand–Prince 5(4)
(`Solver::Rk45`, JSON `rk45`) advance continuous states simultaneously. Rk45
uses internal accepted/rejected substeps while preserving the requested output
grid. Its maximum component error is scaled by `absolute_tolerance +
relative_tolerance * max(abs(old), abs(new))`, defaulting to 1e-9 and 1e-6.
These are local error targets, not a global-error guarantee. It stops at output
boundaries rather than interpolating dense output. Defaults allow 100,000 total
internal attempts (accepted plus rejected), with a hard cap of 1,000,000. UnitDelay updates once per requested step after solver stages.
The initial sample is recorded, and the final step may be shorter to reach the
requested stop time. Step discontinuities must align with the sampling grid.
The solver uses the left limit at a transition when integrating the preceding
interval. Time is in seconds, sine frequency is radians/second.

Options explicitly override imported solver configuration. This is a supported
subset, not a claim of general Simulink numerical equivalence. It rejects
algebraic loops, missing/multiple drivers, non-finite signals, unknown block
types, atomic/conditional subsystems, masks, library links, matrices/vectors, integer types,
external resets and multirate sampling. Root Inports require explicit sources
and currently reject. UnitDelay sample time must be inherited or equal to the
requested step; the stop time must lie on its sampling grid. Workspace names overriding built-in constants reject to avoid
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
