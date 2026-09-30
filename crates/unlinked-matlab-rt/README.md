# unlinked-matlab-rt

Reusable numeric helpers for MATLAB-to-Rust generated programs. Locals and
function arguments remain Rust `f64`, `bool`, `String`, or
`ndarray::ArrayD<f64>`, `ArrayD<bool>`, and `ArrayD<u8>` (ASCII character matrices).
The crate requires Rust `std` (including I/O formatting); it is not a `no_std`
library. Dependencies are `ndarray` 0.17.2 and `nalgebra` 0.35.0 with its `std`
feature. Both native and `wasm32-unknown-unknown` targets compile. The crate has
no interpreter dependency, variable environment, statement counter,
or recursion tracker. Its private compatibility kernels preserve MATLAB indexing,
formatting and broadcasting semantics in a reusable crate.

`ndarray` owns both public and internal arrays: the internal canonical storage
is a Fortran-order `Array2<f64>` with an element-kind tag. Dimensions derive
from ndarray; there is no separate Vec-backed array representation. Temporary
vectors are used for conversions, index lists and formatting. `nalgebra` supplies dense multiplication, pivoted LU
solves, inversion and determinants. Conversion explicitly traverses coordinates
in column-major MATLAB order, including ndarray values with nonstandard strides.
Current helpers reject rank greater than two; rank-one Rust arrays are row vectors.
MATLAB indices are one-based. Colon indexing, logical masks, `end`, scalar
expansion, empty shapes and zero-filled indexed growth follow the supported
interpreter subset. Generated programs are trusted native code: helpers impose
no interpreter statement, element-count, operation-count or formatted-output
budgets. Checked shape arithmetic, address-space representability, index bounds,
and domain validation remain. Scalar indexing and in-bounds numeric scalar
assignment access ndarray storage directly; `end` and truth checks borrow their
inputs. `range_iter` supplies lazy numeric colon iteration without allocating the
range or a vector of column copies.

```rust
use ndarray::ArrayD;
use unlinked_matlab_rt as rt;

fn product() -> rt::Result<ArrayD<f64>> {
    let a: ArrayD<f64> = rt::array(2, 2, vec![1.0, 3.0, 2.0, 4.0])?;
    let identity: ArrayD<f64> = rt::eye((&2.0,))?;
    rt::mtimes(&a, &identity)
}
```

Named helpers such as `add`, `mtimes`, `sum` and `eye` select their result type
statically. Scalar/array operators take borrowed inputs; variable-arity builtins
take typed tuples (`sum((&a,))`, `zeros((&rows, &cols))`). Generated call sites do
not require operator-name strings or dynamic trait objects. `convert::<T>` checks
scalar shape or converts array element kinds. `ArrayD<u8>` preserves character
matrices; `String` accepts only ASCII character rows. `assign` changes its target
only if the complete assignment succeeds. Errors use the public `Error` enum
(`UndefinedVariable`, `UnsupportedRank`, `Shape`, or `Semantic`); private legacy
kernel diagnostics are preserved honestly as `Semantic` messages.
