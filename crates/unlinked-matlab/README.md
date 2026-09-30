# unlinked-matlab

MATLAB/Octave frontend for supported real scalar, matrix and
character-array programs targeting Rust and LLVM. The library
parses input and generates source without executing it. Explicit bounded APIs
also evaluate pure expressions and restricted initialization scripts in process.
The library never launches a compiler, loads files, or provides operating-system
builtins. It is suitable for use from a WASM application or a server.

The parsers support `%` line comments and nested `%{` / `%}` block comments
(up to 64 levels). Block delimiters must occupy standalone lines; malformed,
unmatched and unclosed delimiters produce diagnostics. Commented code is never
parsed or executed, and diagnostic line numbers retain the original source lines.

```rust
use std::collections::BTreeMap;
use unlinked_matlab::{eval_expr, transpile_typed};
let value = eval_expr("gain * sin(pi/2)", &BTreeMap::from([("gain".into(), 2.0)]))?;
assert_eq!(value, 2.0);
let rust = transpile_typed("x = 2^3; disp(x);", false)?;
# Ok::<(), unlinked_matlab::Error>(())
```

## Typed Cargo output

The browser and `unlinked transpile` CLI use `generate_project(source, library)`.
It returns compact typed Rust, a Cargo manifest requiring **both ndarray and
nalgebra**, and `files()` containing a portable vendored `unlinked-matlab-rt`
helper crate. `transpile_typed(source, library)` returns just the Rust source.
Generation does not run MATLAB, Cargo, or the generated program.

```sh
cargo run -p unlinked-cli -- transpile example.m -o example-rust
cargo run --manifest-path example-rust/Cargo.toml
cargo run -p unlinked-cli -- transpile functions.m --library -o functions-rust
cargo run -p unlinked-cli -- transpile functions.m --library --emit llvm-ir -o functions.ll
```

Typed generation uses primitive `f64`/`bool` when proven and `ndarray::ArrayD`
for arrays, with MATLAB column-major indexing and logical element types. Function
parameters without type declarations conservatively accept borrowed numeric arrays.
Outputs are typed values or tuples, and script outputs have named fields for
persistent bindings. Unobserved loop-only temporaries may be omitted from
`ScriptOutput`, allowing scalar range loops without materializing an array.
Potentially unassigned locals report an error when read; they never silently
become zero. Incompatible type changes and unresolved function signatures produce
diagnostics. This is conservative inference, not arbitrary MATLAB type inference.

The helper owns one-based indexing, implicit expansion, growth with zero fill,
empty shapes and formatting. Matrix multiplication/solves/inverse/determinant
use nalgebra. Conversions preserve coordinates even when callers pass C-order
or non-contiguous ndarray storage. The current semantic subset remains real 2D;
ArrayD provides N-D headroom without implying support for all N-D MATLAB operations.

Generated programs have **no environment map or statement-budget machinery**.
They are intended for trusted standalone execution; helpers retain shape, index
and checked dimension arithmetic. The bounded interpreter used by server simulations
is unchanged. Native CLI LLVM generation runs `cargo rustc --emit=llvm-ir` on the
emitted project, which may fetch/build its dependencies; it never runs the resulting
program. LLVM output is one crate module with external dependency declarations;
retain the Cargo project for linking. The browser downloads a complete project ZIP
under a single `generated-matlab/` directory.

## Supported semantics

`transpile_typed(source, library)` emits typed Rust. `eval_expr`,
`eval_array_expr`, `eval_script` and `eval_function` remain separate bounded
reference evaluators for model execution.

The typed compiler supports real numeric/logical arrays, ASCII character data,
scalar and implicit expansion, concatenation, matrix multiplication and square
nonsingular solves, integer matrix powers, one-based indexing, `end`, colon,
logical masks, zero-filled assignment growth, and explicit control flow. Local
functions have typed parameters and tuple outputs. Unsupported type joins
(including logical-to-numeric reassignment, which changes indexing meaning)
produce diagnostics. Typed compilation accepts a restricted MATLAB `arguments`
block with `double`, `logical`, or `char` inputs and two dimensions written as
positive integer literals or `:`. A `(1,1) double` input becomes `f64`; array
dimensions are checked at the function boundary. Defaults, validators, and block
attributes are rejected. The bounded interpreter rejects arguments blocks.
Without declarations, input element types may be proven from calls within a
script; otherwise parameters default to borrowed numeric arrays.

Builtins include elementwise mathematics, constructors, shape/size queries,
reductions, sorting, character formatting, assertions, and display. `inv` and
`det` are available in typed output through nalgebra; the reference interpreter
still uses its established builtin subset. Unsupported complex, sparse, cell,
structure, object, file/process/network, and dynamic-dispatch semantics reject.
`fprintf` accepts a format string, not an external file handle. Bare command-window
echo is unsupported; use explicit `disp` or `fprintf`.

Parser limits are 256 KiB source, 16384 tokens, 64 statement nesting levels,
and 256 expression levels/operators. Semantic helpers validate shapes/indices
and use checked dimension arithmetic. Generated code has no statement or recursion
fuel; it is not a sandbox. The browser accepts at most 64 KiB source and never
executes the result.

Differential tests compile complete Cargo projects and compare array values,
shapes, control flow, indexing and original licensed corpus functions against
the unchanged evaluator and Octave when installed. For corpus functions with
diagnostic printing, the evaluator comparison removes only those printing
statements; generated code and Octave run the original function. The configured corpus path is
`UNLINKED_TEST_CASES`. Compilation or a configured missing corpus is an error;
Octave absence is an explicit skip. One documented divergence is matrix logical
indexing with a row mask: MATLAB/reference semantics produce a column vector,
while Octave can produce a row vector. A false scalar mask on a row array
preserves a `1×0` result here, while Octave returns `0×0`. Tests check these
divergences explicitly instead of claiming shared shape conformance. Shared
kernel consolidation and remaining output/formatting/assignment gaps are tracked
in [issue #25](https://github.com/meawoppl/unlinked/issues/25).

## Portability and extraction

Exported Cargo projects depend on both ndarray and nalgebra with compatible version requirements;
`matlab-rt/` is vendored source and has no dependency on the Unlinked application,
database, interpreter, or server. Copy the entire project to another repository,
or integrate its `src/lib.rs` plus the helper crate and dependency declarations.
The `[workspace]` section keeps standalone exports independent of an enclosing
Cargo workspace; remove it when deliberately adding the package as a workspace
member. Cargo resolves dependencies and writes a lockfile on the first build. The
manifest declares Rust 1.89 or newer, matching nalgebra’s minimum version.

Generated functions expose Rust primitives and borrowed ndarray arrays, with typed
`rt::Error` results. They use named operation helpers rather than runtime
operator-name strings. Source is formatted in-process for extraction and review. The helper's canonical array storage is ndarray; dense linear algebra uses
nalgebra. MATLAB-specific conversion and indexing use small helper calls. Standard Rust error results report unsupported
shapes and invalid operations. Current code requires `std` (including formatting
and explicit printing) and is **not no_std**. The helper compiles for wasm32, but
embedding hosts must decide how to handle console output and trusted execution.

## Bounded initialization scripts

`eval_script(source, &Environment) -> Result<Environment, Error>` interprets a
restricted initialization script in process, returning a new workspace. It never
compiles code or launches a process. `eval_script_with_budget` also accepts
`&mut ArrayBudget`, allowing initialization and subsequent model-parameter
expressions to share one aggregate budget. Failures leave the original workspace
unchanged.

Supported statements are assignments, indexed assignments/growth, `if`/`elseif`/
`else`, column-wise `for`, `while`, and loop-local `break`/`continue`. Expressions
use the existing pure array evaluator. Functions, multiple-output assignments,
`return`, standalone call statements, printing/formatting, and file/process/network
builtins are rejected. Unsupported capabilities are checked in unexecuted branches
too. No files are loaded automatically; callers must explicitly supply script text
and initial variables.

The initialization workspace is limited to 256 variables, 1024 elements per stored
value, and 100000 stored elements total. Names are at most 63 ASCII letters, digits
or underscores, beginning with a letter. Execution stops after 100000 statement
and loop-condition steps, including empty loop bodies. Intermediate expressions
retain their existing one-million-element array cap, eight-million-element
aggregate default, and twenty-million estimated-operation default. Source/token/
parser nesting limits also apply. These limits cover one interpreter invocation;
callers remain responsible for request concurrency and aggregate server load.

Tests check initialization against eight fixed Octave scripts, empty and nested
loops, indexed growth, forbidden capabilities in dead branches, atomic errors,
workspace limits, runaway loops and budgets shared with later parameter evaluation.

Initialization values also cap each dimension at 1,024, including empty arrays.
Shared `ArrayBudget` work charges include dimensions as well as elements, so
zero-element matrices cannot bypass operation limits. Install
`ArrayBudget::with_cancellation(|| interrupted_or_deadline_reached)` for server
execution: returning `true` produces `execution interrupted`. The check runs at
entry and each expression/statement/work charge; individual bounded runtime
operations finish before the next cooperative check. The library itself does
not read clocks or start threads.

`FunctionProgram::parse(source)` validates a pure function file once; its
`signature()` exposes the primary function's name and ordered input/output names.
`evaluate(Vec<Value>)` returns outputs in declaration order. Use
`evaluate_with_budget(args, &mut budget)` to share limits and cancellation across
simulation samples. `eval_function(source, args)` is the parse-and-evaluate helper.
Shapes are checked at runtime; introspection does not infer MATLAB datatypes or
array dimensions. Each call receives its own local workspace, with no persistent
or global state. Local functions, bounded recursion, multiple outputs, array
indexing, loops and early return are supported. `error` and `assert` report failures,
and `sprintf` returns text; printing, files, processes and unknown functions reject
even in dead branches. Initialization scripts still reject function definitions.

Function execution shares the array budgets and initialization value limits,
allows 64 definitions, 256 declared inputs/outputs, and 100,000 statement/loop
steps across all local calls. Combined syntax and call nesting is bounded to
prevent deeply nested recursive expressions from exhausting the host stack.
Unassigned outputs fail explicitly. Corpus interpreter tests execute four original
function files directly; five files containing printing are asserted to reject.
For numeric comparison only, test fixtures remove standalone printing statements
from those five files and compare results to the unchanged originals in Octave.
Production evaluation never strips statements.
