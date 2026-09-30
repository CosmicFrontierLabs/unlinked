# Unlinked — Plan and status

Unlinked is a web-based, pure-Rust tool for Simulink models. It opens existing
`.slx` / `.mdl` files in the browser, renders and edits them, versions them
per organization, and simulates a **bounded, explicitly rejected-otherwise
subset** of Simulink. It also includes a MATLAB/Octave → Rust transpiler
(LLVM IR via `rustc`), whose evaluator doubles as the block-parameter and
init-script interpreter.

It is not a full Simulink or MATLAB replacement. Anything outside the
supported subsets fails with an explicit diagnostic rather than an
approximation; the roadmap below lists what is not supported yet.

The backbone is [single-binary-rust-website](https://github.com/meawoppl/single-binary-rust-website):
an Axum backend with the Yew/Trunk frontend embedded via `memory-serve`,
Diesel/Postgres with embedded migrations, typed WebSockets via `ws-bridge`,
and a single Docker image.

## Workspace layout

```
shared/                    # HTTP/WS API types (serde), shared by backend + frontend
backend/                   # Axum: OAuth, orgs/projects, versioned files, sim jobs
frontend/                  # Yew SPA: viewer/editor, compare, simulate, transpile
crates/
  unlinked-model/          # Model IR, Stateflow IR, diff, edits (wasm-safe)
  unlinked-import/         # .slx / .mdl → IR; lossless patching of edits back
  unlinked-render/         # IR → SVG, diagrams and Stateflow charts (wasm-safe)
  unlinked-matlab/         # MATLAB lexer/parser, bounded evaluator, Rust codegen
  unlinked-sim/            # graph lowering, scheduler, solvers
  unlinked-cli/            # `unlinked info | render | sim | transpile`
```

Model, import, render, matlab and sim all build for `wasm32-unknown-unknown`;
the browser parses, renders, diffs, edits and transpiles locally. Test models
live in the separate [unlinked-test-cases](https://github.com/meawoppl/unlinked-test-cases)
repo (30 third-party models, 1 synthetic model and 9 `.m` scripts), read
from `UNLINKED_TEST_CASES`; corpus tests skip when it is absent and run in
CI. Per-model import, render and simulation coverage is tracked in
[`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).

## Delivered

### Import and IR
- SLX (OPC zip, split `systems/*.xml` parts, `System Ref` resolution) and
  legacy MDL (including windows-1252 files and embedded OPC tails), with byte,
  node and nesting budgets. All 31 corpus models import and render.
- Blocks, lines with branches, masks (both mask formats), library links,
  annotations, solver configuration and model workspace.
- Stateflow charts and MATLAB Function blocks (`stateflow.xml` and the split
  `stateflow/` layout, and the MDL `Stateflow` section): states, transitions,
  junctions, MATLAB code, data declarations and raw timing metadata.

### Rendering and viewing
- SVG rendering of every corpus diagram level (common block glyphs, masks'
  simple display text, ports, routed lines), light and dark themes.
- Stateflow charts and subcharts; MATLAB Function code listings.
- Browser viewer: pan/zoom, subsystem/chart drill-down, tree, breadcrumbs,
  block inspector. Local files are parsed in the browser and never uploaded.

### Web application
- OAuth (Google, GitHub) with PKCE and server-checked flow expiry; dev mode.
- Organizations, projects, per-project roles (viewer / editor / owner), org
  default access, audit log. Authorization goes through one access layer;
  resources without a role answer 404.
- Versioned file storage: immutable versions, history, download, rename,
  soft delete, upload limits.
- Version comparison: structural diff of blocks (added, removed, parameters,
  layout, appearance, ports, library links), wiring, configuration,
  workspace and charts. Block changes are highlighted on the diagram; the
  other changes, including charts, are listed in the change summary.
- Diagram editing: move, rename, re-parameterize and delete blocks, saved as a
  new version by patching the original file rather than regenerating it.
  Untouched SLX parts and a no-op save are byte-identical; an edited XML part
  keeps its unmodeled content but may reserialize attribute quoting.

### Simulation
- Bounded, version-pinned simulation jobs over HTTP and a WebSocket stream,
  with per-user and global caps, quotas, cancellation, deadlines and stored
  results. Live plots use [rizzma](https://crates.io/crates/rizzma) with
  labeled time and value axes; traces export as CSV.
- Solvers: Euler, RK4, adaptive Dormand–Prince 5(4).
- Blocks: sources (Constant, Clock, Step, Sine), Gain, Bias, Sum, Product,
  Saturation, Integrator, UnitDelay, Abs, math/trig, relational and logical
  operators, Switch (`u2 ~= 0`), Mux/Demux, virtual subsystems, proper SISO
  TransferFcn, SISO StateSpace, DiscreteTransferFcn; fixed-size vector and
  matrix signals lowered to scalars. The authoritative list and its limits are
  in [`crates/unlinked-sim/README.md`](crates/unlinked-sim/README.md).
- Multirate discrete scheduling on integer sample ticks (held values between
  hits and through solver stages), and local Goto/From routing.
- Pure scalar MATLAB Function charts execute through a bounded function
  interpreter; anything stateful, non-scalar, complex, variable-size or with
  its own scheduling is rejected.
- Root inputs can be bound explicitly (`unlinked sim --input-value`).
- Coverage: all 31 corpus models import and render. With no inputs or
  overrides only the synthetic reference model compiles for simulation; the
  AutoLayout models run with explicit root inputs, and a real MATLAB Function
  block runs in isolation. See [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).
- Workspace from expressions and from a same-project `.m` init script, chosen
  in the Simulate tab, pinned to its version and evaluated by the bounded
  interpreter; explicit workspace entries override init values.

### MATLAB / Octave
- Array-first evaluator and Rust code generator for a bounded subset: real
  matrices, indexing, control flow, local functions, common builtins. Checked
  against Octave on the corpus scripts. LLVM IR comes from compiling the
  generated Rust with `rustc` (CLI only); there is no in-process LLVM backend.
- In-browser MATLAB → Rust page, also available as a tab on project `.m`
  files.
- Details: [`crates/unlinked-matlab/README.md`](crates/unlinked-matlab/README.md).

## Roadmap (not supported yet)

These are deliberately out of scope today and are rejected when encountered:

- **Stateflow execution** of state charts (states, transitions, events, temporal
  logic). Charts import, render and diff, but only pure scalar MATLAB Function
  charts simulate.
- **Whole-model coverage**: most real corpus models still need unsupported
  blocks, masks, libraries or external inputs before they can run.
- **General masks and toolboxes**: mask initialization code, masked library
  internals that need MathWorks libraries, Simscape and other toolboxes.
- **Full MATLAB language**: cells, structs, classes, function handles, strings
  beyond character arrays, file and OS builtins, `eval`-style dynamic code.
- **Simulation semantics**: zero-crossing detection, variable-step solvers
  other than Dormand–Prince, algebraic loop solving, triggered/enabled and
  function-call subsystems, buses, MIMO state-space, fixed-point types,
  multi-instance model references.
- **Editing**: adding blocks, drawing lines, renaming/deleting blocks that
  own Stateflow charts, and editing chart contents.
- **Scale-out**: simulation caps are per backend instance.

## Conventions
- Work lands as reviewed PRs on `meawoppl/<topic>` branches; squash merge once
  CI (fmt, clippy `-Dwarnings`, tests with Postgres and the corpus, audit,
  release build, container) is green.
- All API/WS types live in `shared` with serde round-trip tests; migrations
  are embedded and applied at startup.
- Untrusted input (models, scripts, edits) is bounded by explicit size and
  work budgets on both the browser and server paths. Server simulation jobs
  also have cooperative deadlines and cancellation; browser import and
  transpilation have no wall-clock deadline.
