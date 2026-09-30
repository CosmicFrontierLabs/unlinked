# unlinked CLI

A local command-line entry point for model import, SVG rendering, scalar simulation, and MATLAB
transpilation. Files and stdin (`-`) are limited to 16 MiB before import; SLX also
has the import crate's decompressed-size and nesting limits.

```sh
cargo run -p unlinked-cli -- info model.slx
cargo run -p unlinked-cli -- render model.slx -o diagram.svg
cargo run -p unlinked-cli -- render model.mdl --system 'Outer/Slash' --system Inner -o nested.svg
cargo run -p unlinked-cli -- sim model.mdl --start 0 --stop 2 --step 0.01 --solver rk4 -o trace.json
cargo run -p unlinked-cli -- sim model.slx --stop 2 --step 0.01 --solver euler -o trace.csv
cargo run -p unlinked-cli -- transpile script.m -o generated.rs
cargo run -p unlinked-cli -- transpile function.m --library --emit llvm-ir -o generated.ll
```

`info` prints model metadata, imported solver settings, per-system statistics,
block-type counts, library-link and mask counts. It does not claim every imported
block is executable. Missing model workspace scripts are not executed.

`render` produces standalone SVG with an explicit dark theme. Without
`--system` it renders the root diagram. Each repeated `--system BLOCK_NAME`
selects one nested subsystem by its exact block name: slashes inside names remain
literal, so `--system 'A/B' --system C` navigates into block `A/B`, then block `C`.
Missing systems fail explicitly. Text and annotations are escaped by the renderer.

`sim` requires explicit stop time, step size and solver. Start time defaults to
zero. These settings override imported solver settings. JSON includes the chosen
options and imported configuration, plus a trace keyed by stable block ID. CSV
contains a `time` column followed by block-ID columns in sorted order; standard
CSV escaping preserves punctuation in identifiers. `.csv` output paths select
CSV automatically; `--format json|csv` overrides the choice. Without `--output`,
results go to stdout; diagnostics and the selected solver go to stderr.

Use repeatable `--var NAME=EXPR` to supply scalar model workspace values without
executing an external initialization script. For example, `--var 'gain=base*2'
--var base=3` resolves dependencies and supplies gain 6. Names use at most 63
ASCII letters, digits and underscores, starting with a letter; MATLAB keywords
are rejected. Duplicate names (including imported workspace entries), empty or
unresolved expressions fail explicitly. Built-in constant overrides are rejected
by the simulation engine. JSON output records the raw workspace expressions for
reproducibility. Scripts in the model's directory are never run automatically.

Unsupported simulation semantics fail with a diagnostic. The simulation engine
limits sample counts and total recorded values. `--max-samples` defaults to
100001 and cannot exceed the engine's hard maximum.

`transpile` emits the documented scalar subset from `unlinked-matlab`. Function
library mode exposes `pub fn f_name(...) -> f64` without `main`. `--emit llvm-ir`
requires an output file and local `rustc`; it compiles only generated Rust and
never runs it. There are no user-selected compiler flags, external dependencies,
build scripts, or procedural macros in generated code. This is a local CLI, not
an uploaded-program execution service.

Output files are written only after successful import/rendering/simulation/transpilation.
Explicit output paths may overwrite existing files. LLVM output is written by
`rustc` itself and compiler failures are reported with its diagnostics.

The optional external-corpus CLI regression uses `UNLINKED_TEST_CASES` to locate
`fixtures/synthetic/constant_gain_sum_integrator.mdl` and its expected JSON. It
compares all recorded times and named oracle signals within the oracle tolerance.
The oracle is an authored analytic solution, not a captured Simulink execution.
When the environment variable is absent the test explicitly reports a skip; a
configured missing or invalid corpus fails the test.
