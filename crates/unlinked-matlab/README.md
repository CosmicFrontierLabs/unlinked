# unlinked-matlab

Dependency-free MATLAB/Octave frontend for supported real scalar, matrix and
character-array programs targeting Rust and LLVM. The library
parses input and generates source; it never executes input, launches a compiler,
loads files, or provides operating-system builtins. It is suitable for use from a
WASM application or a server.

```rust
use std::collections::BTreeMap;
use unlinked_matlab::{eval_expr, transpile};
let value = eval_expr("gain * sin(pi/2)", &BTreeMap::from([("gain".into(), 2.0)]))?;
assert_eq!(value, 2.0);
let rust = transpile("x = 2^3; disp(x);")?;
# Ok::<(), unlinked_matlab::Error>(())
```

For a trusted local script:

```sh
cargo run -p unlinked-matlab < example.m > example.rs
rustc --edition=2024 --emit=link,llvm-ir example.rs
./example
```

Function-only files can become a callable library using `transpile_library(source)`
or the CLI's `--library` option. A MATLAB function `polynomial(x)` becomes
`pub fn f_polynomial(v_x: f64) -> f64`; generated code has no `main`. Compile it
with `rustc --crate-type=lib --emit=link,llvm-ir generated.rs`, or include it as a
Rust module and call `generated::f_polynomial(3.0)`. Library mode rejects script
statements and empty files.

The CLI reads stdin and writes Rust to stdout. `rustc` produces native code and
LLVM IR; this is an explicit local compilation workflow, not a compiler service
for uploaded programs. Production execution needs process isolation, resource
limits and a job queue. Generated functions can recurse; a bounded individual
range does not bound total runtime or output.

## Original scalar frontend

- Real `f64` scalar literals, scientific notation, named variables and `%` comments.
- Assignments terminated by newline or semicolon and explicit `disp(expression)`.
- `+ - * / ^`, unary `+ - ~`, comparisons and short-circuit `&& ||`.
- MATLAB precedence: `-2^2` evaluates to `-4`; chained powers associate left.
- `pi`, `Inf`, `NaN`, `true`, `false` (variables may override these constants).
- Scalar `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sqrt`, `abs`,
  `exp`, `log`, `log10`, `floor`, `ceil`, `round`, `sign`, `min`, `max`, `mod`.
- `if` / `elseif` / `else` / `end` and `for i = start:step:stop` / `end`;
  the step defaults to one. Range bounds are evaluated once.
- Local, single-output `function y = f(x, ...)` definitions after the script,
  with explicit `end`. Parameters and function variables have local scope.
- NaN-to-logical conversions report errors (runtime assertions in generated
  programs); short-circuit operators skip their unevaluated operand.
- `mod` snaps quotients within two floating-point epsilon units of an integer,
  including the decimal-boundary case `mod(0.3, 0.1) == 0`.
- `eval_expr(source, workspace)` evaluates the same scalar expression subset
  directly against a `BTreeMap<String, f64>` for model block parameters.

## Original scalar-frontend limitations

This is not a complete MATLAB compatibility implementation. In the original
scalar frontend, arrays, complex
numbers, indexing, strings, cells, structures, matrix operators, scripts calling
other files, anonymous functions, closures, classes, multiple outputs, `while`,
`break`, `return`, `global`, `persistent` and built-in shadowing are unsupported.
Bare expressions and automatic command-window output are not implemented.
Assignments without semicolons do not print. Syntax and unknown functions are
rejected, not guessed. Generated code checks use-before-assignment, including
branches and potentially empty loops. Newly initialized loop variables are not
considered definitely assigned after the loop, even for constant positive ranges.

The numeric model is Rust `f64`, not MATLAB array arithmetic: NaN behavior in
`min`/`max`, overflow, underflow, rounding near colon endpoints, transcendental
functions and `mod` can differ from MATLAB. Complex-valued operations yield NaN.
Ranges must be finite, have a nonzero step and contain between one and one million
iterations. Empty ranges are rejected at runtime: MATLAB/Octave assign an empty
array to the loop variable even when it previously contained a scalar; preserving
that scalar would silently produce the wrong result. Inputs are limited to 64 KiB, 1024 tokens and 64 nested parser levels
for this initial subset. Diagnostic line numbers are exact for lexer/parser
errors; semantic diagnostics currently point to line 1.

Tests cover rejection of unsupported input, scalar evaluation, control flow,
local functions, actual compilation/execution of a fixed repository fixture and
LLVM IR emission, exported function-library calls, and 2560 deterministic malformed
inputs. If Octave is installed, differential tests compare 24 fixed expressions
against both the direct evaluator and compiled Rust output. If unavailable, the
test reports that the differential check was skipped. Tests never compile
user-provided uploaded input.

Logical-conversion behavior follows [MathWorks logical documentation](https://www.mathworks.com/help/matlab/ref/logical.html).
Floating-point modulo handling is informed by [GNU Octave arithmetic documentation](https://docs.octave.org/latest/Utility-Functions.html).
The differential test is a compatibility check for the listed subset, not a claim
of full MATLAB or Octave conformance.

## Real matrix and character-array frontend

`transpile` and `transpile_library` now select a native array frontend for matrix
literals, ranges assigned as values, indexed variables/parameters, character
literals, array builtins, elementwise/matrix operators, or `while`/`break`/
`continue`/`return`. Sources requiring only the original scalar subset retain its
primitive `f64` function ABI. `transpile_arrays(source, library)` explicitly selects
the array frontend for any supported program. Selection does not execute input or
silently substitute an unknown function.

This frontend generates ordinary Rust control flow and function calls backed by
a dependency-free value runtime. It does not embed a MATLAB source interpreter,
invoke Octave, or execute operating-system commands. The generated Rust can be
compiled to a native executable or LLVM IR with the same `rustc` workflow.

```rust
use std::collections::BTreeMap;
use unlinked_matlab::{eval_array_expr, array_runtime::Value};
let workspace = BTreeMap::from([("gain".into(), Value::scalar(2.0))]);
let vector = eval_array_expr("gain * [1; 2; 3]", &workspace)?;
assert_eq!((vector.rows, vector.cols), (3, 1));
assert_eq!(vector.data, vec![2.0, 4.0, 6.0]);
# Ok::<(), unlinked_matlab::Error>(())
```

`eval_array_expr(source, &BTreeMap<String, Value>)` evaluates only pure parameter
expressions. It cannot print, open files, spawn processes, execute scripts, or call
user functions. Scalar `eval_expr` remains unchanged for existing simulation
callers. Pure array evaluation additionally caps aggregate intermediate values at
eight million elements and estimated numeric work at twenty million operations
per expression; these budgets prevent repeated large subexpressions from evading
the individual array limits. `Value` exposes `rows`, `cols`, `data` in **column-major** order, and
`kind` (`Numeric`, `Logical`, or `Character`). `Value::new`, `Value::row`, and
`Value::scalar` construct inputs; `validate` checks externally constructed values.

Array-library exports use `pub fn f_name(args: Vec<Value>) ->
ArrayResult<Vec<Value>>`. Arguments follow declaration order, and results follow
the declared output list. This ABI supports dynamic shapes and multiple outputs.
Function calls share statement/loop and recursion budgets. Function outputs not
assigned on the executed path return an error. Scalar-only library exports retain
the earlier `f_name(f64, ...) -> f64` ABI.

Supported array behavior:

- Real, two-dimensional matrices/vectors, `[]`, row/column concatenation, scalar
  expansion and two-dimensional implicit expansion for elementwise operators.
- Column-major one-based indexing, two-subscript Cartesian indexing, logical
  masks, `end`, `:`, and colon ranges; indexed assignments, scalar expansion,
  vector growth and explicit two-dimensional growth. Deletion remains unsupported.
- Matrix multiplication, transpose (`'` and `.'` for real values), square
  nonsingular left/right division, integer square-matrix powers, and `.*`, `./`,
  `.\`, `.^`. Complex results and general matrix functions fail explicitly.
- MATLAB whitespace-sensitive matrix literals, `%` comments and `...` line
  continuations. Character literals support doubled apostrophes and ASCII only.
- `if` / `elseif` / `else`, `for` over columns, `while`, `break`, `continue`, local
  functions, `return`, and multiple output assignment. Empty array-loop ranges
  correctly assign an empty loop variable; the older scalar-only range path
  still rejects this array-valued result explicitly.
- Scalar/elementwise math from the original frontend plus `log2`, `rem`,
  `isnan`, `isinf`, `isfinite`; `zeros`, `ones`, `eye`, `reshape`, `size`, `length`,
  `numel`, `isempty`, `linspace`, `diag`, `sum`, `prod`, `all`, `any`, `min`, `max`,
  vector `norm`/`dot`/`sort`, `find`, `transpose`, and `strcmp`.
- `disp`, scalar `num2str`, `fprintf`, `sprintf`, `error`, and `assert` for locally
  compiled programs. Formatting supports `%g`, `%f`, `%e`, `%d`/`%i`, `%s`, `%%`,
  bounded width/precision, and newline/tab/carriage-return/backslash escapes.
  Numeric display/formatting aims at useful values, not byte-for-byte MATLAB
  command-window formatting. `fprintf` accepts a format string, never a file ID.

Array-subset limits are 256 KiB source, 16384 tokens per source, 512 tokens and
256 operators per expression, 64 parser/call nesting levels, one million elements
per array/dimension, one million executed statements/loop iterations, ten million
operations per matrix multiplication/solve, and four MB per formatted string.
These are finite guardrails, **not** a total CPU/memory sandbox: callers executing
compiled programs still need process-level isolation and resource limits.

Unsupported array features include complex/sparse/N-dimensional arrays, cell and
structure values, object classes, function handles, closures, globals, script
loading, file/process/network functions, automatic command-window echo,
`nargin`/`nargout`, arbitrary integer types, matrix-index deletion, linear growth
of a non-vector matrix, non-square least-squares solves, fractional matrix powers,
and indexing a temporary expression directly. Assign it to a variable first.

When Octave is available, tests compare 55 fixed array expressions against both
pure evaluation and generated Rust, including array shapes and column-major
values. With `UNLINKED_TEST_CASES` set, nine licensed corpus function files compile
and match Octave across eleven cases: rotation, skew matrices, binary/linear
search, palindrome detection, bubble sort, Euclidean distance, factorial and
Fibonacci. Tests also exercise generated loops/multiple outputs, index/runtime
errors, parser fuzz cases and execution budgets. Missing optional prerequisites
produce explicit skips; a configured missing corpus is an error.

Logical indexing of a matrix produces a column vector, following the
[MathWorks matrix-indexing description](https://www.mathworks.com/company/technical-articles/matrix-indexing-in-matlab.html).
Octave differs for a logical row-vector mask applied to a matrix: it can return
a row vector. That shape corner is not included in the shared-conformance claim.
The special `find([])` and `find(0)` results are 0-by-0, matching the
[MathWorks documented convention](https://www.mathworks.com/help/matlab/ref/find.html).
