# unlinked-sim

A deterministic, bounded simulator with scalar execution and array signal lowering written entirely in Rust, with no
filesystem, network, subprocess or native runtime dependency. `compile` resolves fixed signal dimensions and lowers
`unlinked_model::Model` to a scalar `Graph`; `simulate` runs an explicit graph;
`simulate_model` combines both. Trace JSON contains sample times, stable block
IDs mapped to scalar sample vectors, and the selected solver.

Implemented: Constant/Ground, Clock, Step, time-based Sine, time-based Pulse
Generator, Signal Generator (sine/square/sawtooth), Ramp, Chirp Signal,
Repeating Sequence, Gain, Bias, Sum/Add,
Product/division, Saturation, the stateless Discontinuities (Dead Zone,
Quantizer, Coulomb & Viscous Friction, Wrap To Zero, Saturation Dynamic, Dead
Zone Dynamic), Integrator, UnitDelay, Abs, the stateless Math
Operations blocks listed under "Math operations" below, scalar relational
comparisons, logical operators, Switch routing, the Signal Routing selection
blocks (Selector, Multiport Switch and Index Vector, Manual Switch, Environment
Controller; see below), and single-input sinks. Logic and relational outputs
use exact 0/1 scalar values, including declared boolean outputs. Explicit zero-crossing
detection on Switch/RelationalOperator/DeadZone rejects; no event root finding is implied. Ordinary virtual subsystems lower
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

Imported MATLAB Function charts lower to those execution nodes when their
escaped model path, declared data, function signature and generated `sf_sfun`
backing wiring agree. The supported imported subset is pure, scalar, fixed-size,
real, double/inherited numeric data with inherited scheduling and one output.
The legacy default transition calling `eML_blk_kernel()` is recognized only in
its generated form. Additional chart actions, graphical state machines,
parameters requiring external workspace semantics, altered backing wiring,
conditional ports, masks and links fail explicitly. Chart-path matching does
not change the imported model. The corpus test isolates the unchanged
`PID Control/ud` function from `rovSim_los.slx` and checks its source formula;
it does not claim that the complete ROV model or its stateful guidance chart runs.

Function execution has a deterministic default budget of 20 million estimated
operations across the entire run, including all solver stages. Adaptive RK45 can
consume more function work than RK4 on the same output grid and may reach this
limit sooner. Trusted native callers can pass an explicit `ArrayBudget` to
`simulate_with_observer_and_budget`; server jobs retain the fixed budget, shared
with initialization, plus the 30-second cooperative deadline and 25,001-sample
cap. Budget exhaustion is an explicit error, never a partial successful trace.

### Commented blocks

Blocks marked `Commented=on` are removed before simulation as Simulink's
[Comment Out](https://www.mathworks.com/help/simulink/ug/comment-out-blocks.html)
does: the signals into them are terminated and every destination their outputs
fed reads Ground (0). Blocks marked `Commented=through` are removed and input
*i* is wired straight to output *i*; an undriven input grounds its outputs.
Both work at every subsystem level and around Goto/From routing (a
commented-out Goto grounds its From blocks), and the removed blocks are never
evaluated or validated: neither their parameters nor anything inside a
commented subsystem, including its routing blocks and MATLAB Function charts.
As in Simulink, comment through is refused for blocks whose input and output
port counts differ or that have control or connection ports (so also for Goto
and From); Inport, Outport, Goto Tag Visibility and Data Store Memory blocks
cannot be commented, and any other `Commented` value is rejected. The renderer
greys commented blocks and their lines, dashes the commented-through path and
shows the `%` or `%%` badge.

Two known divergences from Simulink: the Ground standing in for a
commented-out block is recorded in the trace under that block's own ID (so a
trace equals the edited model's only when the commented block had no connected
outputs; Simulink logs nothing for it), and that Ground is scalar, so a
width-sensitive destination such as an equal-width Demux reports its own width
error where Simulink would back-propagate the destination width.

### Math operations

Elementwise blocks accept scalars, vectors and matrices with scalar expansion
between two inputs. Results that leave the reals (log or sqrt of a negative,
`reciprocal` of zero, `rem(x, 0)`, `acosh` below one) are non-finite signal
errors, never substituted values; complex output types reject.

- **Math Function** (`Operator`): `exp`, `log`, `2^u`, `10^u`, `log10`,
  `magnitude^2`, `square`, `conj` (identity on reals), `reciprocal` (exact
  division only), the two-input `pow`, `hypot`, `rem`, `mod`, and
  `transpose`/`hermitian`. `rem` takes the sign of the dividend and `mod` the
  sign of the divisor with `mod(x, 0) = x`, and both compensate for
  floating-point round-off as MATLAB does (`mod(4*pi, 2*pi)` and `rem(1, 0.1)`
  are exactly 0). `pow` with *Signed power* on (the default) is `sign(u)*|u|^v`
  for both the `auto` and `real` output types, since every signal here is
  real, as the
  [block reference](https://www.mathworks.com/help/simulink/slref/mathfunction.html)
  specifies; with *Signed power* off it is `u^v`. `transpose` and `hermitian`
  (equal on reals) swap the rows and columns of a matrix signal; a 1-D vector
  has no orientation and rejects, so use an oriented `1xN`/`Nx1` Constant
  (`VectorParams1D=off`). `OutputSignalType=complex` and Newton-Raphson
  reciprocals reject.
- **Trigonometric Function** (`Operator`): `sin`, `cos`, `tan`, `asin`, `acos`,
  `atan`, `atan2` (y on port 1, x on port 2), `sinh`, `cosh`, `tanh`, `asinh`,
  `acosh`, `atanh`, and `sincos` with separate sine (port 1) and cosine (port 2)
  outputs traced as `id:out:1` / `id:out:2`. `cos + jsin`, CORDIC/Lookup
  approximations and complex outputs reject.
- **Sqrt** (`Operator`): `sqrt`, `signedSqrt` (`sign(u)*sqrt(|u|)`, defined for
  negatives) and `rSqrt` (`1/sqrt(u)`); exact algorithms only, so the
  Newton-Raphson and Lookup methods reject under both the pre-R2026b
  `AlgorithmType` name and `SqrtAlgorithmType`/`RSqrtAlgorithmType`.
- **Sign**, **Unary Minus**, **Rounding Function** (`floor`, `ceil`, `round`
  with halves away from zero, `fix` toward zero).
- **Polynomial** (`coefs` in descending powers, as `polyval`), evaluated
  elementwise by Horner's rule with up to 1,024 coefficients. A block without
  `coefs` uses the file's `BlockParameterDefaults`, else Simulink's library
  default polynomial; palette blocks are created with an explicit `[1 0]`.
- **Dot Product**: `sum(u1 .* u2)` of two scalars, 1-D vectors or column
  vectors of equal length, lowered to Product terms `id:$dot:k` and a Sum;
  other matrices reject. Expanding a scalar against a vector is an Unlinked
  extension that the block reference does not describe.
- **MinMax** (`Function`, `Inputs`): one input reduces a scalar or 1-D vector
  to a scalar; several inputs compare elementwise with scalar expansion.
  Matrix reduction, `CollapseMode=Specified dimension` and the library
  *MinMax Running Resettable* block reject. Zero-crossing settings are ignored
  as for Abs and Saturation: there is no event root finding.

### Stateless discontinuities

Native `DeadZone` (`LowerValue`/`UpperValue`, defaults -0.5/0.5) outputs zero
inside the zone and the input minus the exceeded edge outside it; `LowerValue`
above `UpperValue` and an explicit `ZeroCross=on` reject; an omitted
`ZeroCross` (Simulink's unwritten default, `on`) is accepted without locating
the edge crossings, as for Switch. `Quantizer` outputs `q * round(u / q)` with
MATLAB's half-away-from-zero rounding and requires a nonzero
`QuantizationInterval` (default 0.5; the reference allows any real value, and a
negative interval quantizes like its magnitude). Coulomb & Viscous Friction
outputs `sign(u) .* (gain .* |u| + offset)` with `sign(0) = 0`; `gain` defaults
to 1 and `offset` to the library's `[1 3 2 0]`. It is accepted only as the
library link `simulink/Discontinuities/Coulomb & Viscous Friction`, the form
Simulink saves; the palette creates that link too. Zone edges, interval, gain
and offset may be arrays: they expand a scalar input and must otherwise match
the signal shape elementwise.

Library links (`Reference` blocks, matched by their `SourceBlock` path with
line breaks collapsed) are lowered for exactly the links the block catalog
describes: Coulomb & Viscous Friction, Wrap To Zero, Saturation Dynamic and
Dead Zone Dynamic. Parameter values are read from the block, falling back to
mask values when the file keeps the mask. Wrap To Zero outputs zero when the
input is strictly above `Threshold` (default 255) and the input otherwise; a
vector threshold must match the input size. Saturation Dynamic takes `up`, `u`,
`lo` on inputs 1 to 3 and outputs `max(lo, min(u, up))`, so a lower limit above
the upper one wins as in the library's switch chain. Dead Zone Dynamic takes
the same inputs and outputs `u - max(lo, min(u, up))`: zero inside the limits,
`u - up` above and `u - lo` below; since the library block is `u` minus a
Saturation Dynamic of the same inputs, crossed limits give `u - lo` just as
that block gives `lo`. None of these blocks registers zero crossings.
Every other library link, and the stateful Backlash, Rate Limiter, Relay and
Hit Crossing blocks, still reject. References: [Dead Zone](https://www.mathworks.com/help/simulink/slref/deadzone.html),
[Quantizer](https://www.mathworks.com/help/simulink/slref/quantizer.html),
[Coulomb and Viscous Friction](https://www.mathworks.com/help/simulink/slref/coulombandviscousfriction.html),
[Wrap To Zero](https://www.mathworks.com/help/simulink/slref/wraptozero.html),
[Saturation Dynamic](https://www.mathworks.com/help/simulink/slref/saturationdynamic.html),
[Dead Zone Dynamic](https://www.mathworks.com/help/simulink/slref/deadzonedynamic.html).

### Zero-order hold

`ZeroOrderHold` samples its input at the initial hit and each subsequent period,
then holds the value through observation samples and all continuous solver
stages. Unlike UnitDelay, it has no one-period delay. Scalar and one-dimensional
vector signals retain their dimensions; matrix signals reject. `SampleTime` must
be explicitly specified (or resolved from imported block defaults). Missing or
inherited (`-1`) sample times reject until rate inference is supported. Periods
must be positive integer multiples of the observation step; offsets and continuous
sample times reject. Sampling is direct-feedthrough at hits, so an algebraic
feedback loop still rejects. See the [block reference](https://www.mathworks.com/help/simulink/slref/zeroorderhold.html).

### Signal routing selection

Selection is resolved during dimensional lowering on the scalar wires, so no
block here adds runtime state except the Multiport Switch's per-step index.
Anything outside the subsets below rejects with a diagnostic naming the block
and parameter; nothing is approximated.

- **Switch** accepts all three criteria. `u2 ~= 0` passes the first input for a
  nonzero control; `u2 >= Threshold` and `u2 > Threshold` compare the control
  and the scalar `Threshold` (default 0; not evaluated for the nonzero
  criterion) as doubles, so the boundary value passes for `>=` and not for `>`.
  An imported block without `Criteria` uses `u2 >= Threshold`, the built-in
  default Simulink writes to `BlockParameterDefaults` (the reference page's
  `u2 > Threshold` default is the library block's, whose copies store the
  criterion explicitly). Simulink gives boolean controls special treatment when a
  threshold is "improper" (ignoring it with a warning); that depends on signal
  datatype propagation and still follows #149, so a boolean control is compared
  as its exact 0/1 value. [Reference](https://www.mathworks.com/help/simulink/slref/switch.html).
- **Selector** supports `NumberOfDimensions` 1 and 2, both `IndexMode`s, and per
  dimension `Select all`, `Index vector (dialog)` (any order, repeats allowed)
  and `Starting index (dialog)` with an output size, read from the
  comma-separated `IndexOptions`/`Indices`/`OutputSizes` that files store or
  the programmatic `IndexOptionArray`/`IndexParamArray`/`OutputSizeArray` cell
  arrays (a block carrying both spellings must agree, since merged file
  defaults cannot be told from the block's own values), plus the legacy
  `Elements`/`InputPortWidth`/`ElementSrc` spelling. A 1-D selection of one
  element is a scalar; otherwise a 1-D vector. A 2-D selection requires a
  matrix input (orientation-less vectors reject) and keeps two dimensions, so
  one selected row is a 1-by-N matrix. Indices are bounds-checked against the
  input at compile time; `InputPortWidth` other than -1 must match the input.
  Every port-driven index option, more than two dimensions and an index port
  reject. [Reference](https://www.mathworks.com/help/simulink/slref/selector.html).
- **Multiport Switch** reads `Inputs`, `DataPortOrder` (`Zero-based contiguous`,
  `One-based contiguous`, `Specify indices` with a `DataPortIndices` cell array;
  the legacy `zeroidx` flag is honored when `DataPortOrder` is absent),
  `DataPortForDefault` (`Last data port` or `Additional data port`) and
  `DiagnosticForDefault`. The control must be scalar and is truncated toward
  zero before matching, as the reference page states. An unmatched control
  value stops the run when the diagnostic is `Error` (the documented default)
  and otherwise selects the default port; `Warning` produces no output
  difference from `None` and no warning is surfaced. Data ports broadcast
  scalars against matching shapes. With one data port the block is an index
  vector: the control selects an element of that scalar or 1-D vector by the
  contiguous order (`Specify indices` and an additional default port reject
  there). `AllowDiffInputSizes=on` rejects.
  [Reference](https://www.mathworks.com/help/simulink/slref/multiportswitch.html).
- **Index Vector** (`simulink/Signal Routing/Index Vector`, a linked `Reference`)
  lowers onto the Multiport Switch with the library's one zero-based data port;
  parameters the link overrides take precedence. Simulink saves copies of this
  unmasked built-in as plain `MultiPortSwitch` blocks, which is also what the
  palette creates; the link form is an import convenience.
  [Reference](https://www.mathworks.com/help/simulink/slref/indexvector.html).
- **Manual Switch** passes its first input for position 1 and its second for
  position 0, with scalar expansion between the inputs. Files store the
  position as `CurrentSetting` (the corpus instance and its file defaults use
  nothing else); the programmatic `sw` is also read and wins when both are
  present. The reference page gives no default, so an absent position rejects,
  as does `varsize=on`.
  [Reference](https://www.mathworks.com/help/simulink/slref/manualswitch.html).
- **Environment Controller** (`simulink/Signal Routing/Environment Controller`,
  a linked `Reference`) passes its Sim input (port 1); both inputs must have
  identical dimensions. MathWorks has retired the block in favor of Variant
  Source, which is not implemented.
  [Reference](https://www.mathworks.com/help/simulink/slref/environmentcontroller.html).

Variant Source/Sink, Vector Concatenate, Signal Conversion and other library
links remain unsupported and reject.

### Time-based sources

`DiscretePulseGenerator` (Pulse Generator) runs only with `PulseType=Time based`
and simulation time as its time source: the output is 0 until `PhaseDelay`,
then `Amplitude` for `Period*PulseWidth/100` seconds of every `Period`, with
the rising edge taking the high value and the falling edge the low one, as in
the parameter-effects figure of the
[block reference](https://www.mathworks.com/help/simulink/slref/pulsegenerator.html).
`Sample based` pulses, an external time signal, constant configurations
(a width of 0 % or 100 %, which Simulink also rejects) and negative phase
delays reject. `SignalGenerator` supports `WaveForm` sine, square and
sawtooth with `Units` `rad/sec` or `Hertz`, following the default-output
figures of the
[block reference](https://www.mathworks.com/help/simulink/slref/signalgenerator.html):
sine is `A sin(ωt)`, square is `-A` over the first half of each period and
`+A` over the second, and sawtooth falls linearly from `+A` at the start of
each period to `-A` at its end. The `random` waveform and an external time
source reject; square and sawtooth need a positive frequency. Both blocks
expand scalar parameters against vector or matrix ones (`VectorParams1D`).

Three library blocks are recognised by their `SourceBlock` path on `Reference`
blocks and lowered from their mask parameters: `simulink/Sources/Ramp`
(`slope`, `start`, and the initial output as `X0` or, in files following the
current reference, `InitialOutput`; the catalog dialog edits whichever key the
block carries; output `X0` until `start`, then `X0 + slope*(t - start)`;
[reference](https://www.mathworks.com/help/simulink/slref/ramp.html)),
`simulink/Sources/Chirp Signal` (`f1`, `T`, `f2`; no phase parameter;
`sin(2π(f1 t + (f2 - f1) t²/(2T)))`, whose instantaneous frequency sweeps
linearly from `f1` to `f2` at `T` and keeps changing at that rate afterwards;
the start time must be 0 as the
[reference](https://www.mathworks.com/help/simulink/slref/chirpsignal.html)
requires) and `simulink/Sources/Repeating Sequence` (`rep_seq_t`, `rep_seq_y`;
linear interpolation repeated with the last time value as the period, per the
[reference](https://www.mathworks.com/help/simulink/slref/repeatingsequence.html);
times must be strictly increasing from 0, since the published algorithm is
stated for sequences starting at zero). Ramp and Chirp parameters expand like
the native sources; a Repeating Sequence is scalar. Any other link or mask
still rejects.

Like Step transitions, every edge of these sources must fall on the sampling
grid for fixed-step runs: the Pulse Generator phase delay must be a sample time
and its period and high time whole numbers of steps (the fixed-step guidance of
its reference); the square half period and the sawtooth period must be whole
numbers of steps with time zero on the grid (the default 1 rad/s period of 2π
therefore rejects for those waveforms; use Hertz or a dividing step); and a
Repeating Sequence whose first and last output values differ needs its period
on the grid. Smooth sources (sine, ramp, chirp, a sequence that wraps
continuously) have no such constraint. At an edge the solver uses the left
limit when integrating the preceding interval, so pulse integrals are exact.

### Random sources

`RandomNumber` and `UniformRandomNumber` produce seeded samples at time zero (or
an aligned simulation start), then hold each value until the next sample hit.
Their period must be a positive integer multiple of the simulation step; absent
`SampleTime` defaults to 0.1 seconds. Inherited/continuous random sample times
are rejected. Gaussian `Mean`/`Variance` default to 0/1, uniform
`Minimum`/`Maximum` to -1/1, and `Seed` to 0. Seeds must be integers in
0..=4294967295, variance finite and nonnegative, and uniform bounds finite with
minimum strictly less than maximum. Parameter arrays use scalar expansion and
a separate stream per element. Equal seeds produce equal standardized streams.

Every run resets each stream; independent blocks do not consume one another's
random state, and RK solver stages never advance it. Unlinked uses SplitMix64
with Box–Muller for Gaussian values. **Exact sample-sequence parity with Simulink
is unverified and is not promised**: MathWorks documents its legacy MATLAB v4
generator. Distribution and hold semantics are tested; this is not a
cryptographic generator. References: [Random Number](https://uk.mathworks.com/help/simulink/slref/randomnumber.html)
and [Uniform Random Number](https://www.mathworks.com/help/simulink/slref/uniformrandomnumber.html).

## Editor diagnostics

`diagnose::diagnose(&Model, &DiagnosticContext)` returns serializable findings
with model, configuration, block (subsystem ID path and optional parameter), or
snapshot-local line targets. `CheckMode::Static` runs structural/catalog checks,
known literal mode restrictions, and missing bare workspace-variable/input
checks without evaluating expressions. It always reports `NotChecked`, never
simulation readiness. Compound expression errors need the optional compile check.

`CheckMode::Compile` requires explicit `Options`; it accepts pure workspace
expression overrides and constant root-input expressions. It never runs model,
mask or initialization callbacks. Supply initialization values explicitly after
an independently authorized evaluation. Imported settings are diagnosed but do
not replace explicit run options. `Compiled` only means compilation accepted the
snapshot and context, not that simulation will finish or be numerically correct.
The compiler reports the first semantic failure; generated/ambiguous IDs fall
back to a model target rather than guessing a highlighted source block.

In browsers, run compile checks in disposable workers and terminate superseded
workers. Tag every result with the request generation; line indices only apply
to that snapshot. Static checks are bounded too, but debouncing/off-thread work
is recommended for large files. Preflight caps are 5,000 blocks, 25,000 line,
branch and vertex objects, depth 32, and 2 MiB serialized model/context. A budget
refusal reports `Incomplete` and `truncated`; omitted warning display is reported
separately. An incomplete report must not be shown as a clean model.
