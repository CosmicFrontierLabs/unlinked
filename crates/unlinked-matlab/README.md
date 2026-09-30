# unlinked-matlab

Dependency-free scalar MATLAB/Octave frontend for Rust and LLVM. The library
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

The CLI reads stdin and writes Rust to stdout. `rustc` produces native code and
LLVM IR; this is an explicit local compilation workflow, not a compiler service
for uploaded programs. Production execution needs process isolation, resource
limits and a job queue. Generated functions can recurse; a bounded individual
range does not bound total runtime or output.

## Supported subset

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
- `eval_expr(source, workspace)` evaluates the same scalar expression subset
  directly against a `BTreeMap<String, f64>` for model block parameters.

## Deliberate limitations

This is not a complete MATLAB compatibility implementation. Arrays, complex
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
Ranges must be finite, have a nonzero step and contain at most one million
iterations. Inputs are limited to 64 KiB, 1024 tokens and 64 nested parser levels
for this initial subset. Diagnostic line numbers are exact for lexer/parser
errors; semantic diagnostics currently point to line 1.

Tests cover rejection of unsupported input, scalar evaluation, control flow,
local functions, actual compilation/execution of a fixed repository fixture and
LLVM IR emission. They never compile user-provided uploaded input.
