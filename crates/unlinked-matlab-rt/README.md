# unlinked-matlab-rt

Reusable numeric helpers for MATLAB-to-Rust generated programs. Locals and
function arguments remain Rust `f64`, `bool`, `String`, or
`ndarray::ArrayD<f64>`, `ArrayD<bool>`, and `ArrayD<u8>` (ASCII character matrices).
The crate requires Rust `std` (including I/O formatting); it is not a `no_std`
library. Dependencies are `ndarray` 0.17.2 and `nalgebra` 0.35.0 with its `std`
feature. Both native and `wasm32-unknown-unknown` targets compile. The crate has
no interpreter dependency, variable environment, statement counter,
or recursion tracker. Its private compatibility kernels preserve MATLAB indexing,
formatting and broadcasting semantics; they are not pasted into generated modules.

`ndarray` owns both public and internal arrays: the internal canonical storage
is a Fortran-order `Array2<f64>` with an element-kind tag. Dimensions derive
from ndarray; there is no separate Vec-backed array representation. Temporary
vectors are used for conversions, index lists and formatting. `nalgebra` supplies dense multiplication, pivoted LU
solves, inversion and determinants. Conversion explicitly traverses coordinates
in column-major MATLAB order, including ndarray values with nonstandard strides.
Current helpers reject rank greater than two; rank-one Rust arrays are row vectors.
MATLAB indices are one-based. Colon indexing, logical masks, `end`, scalar
expansion, empty shapes and zero-filled indexed growth follow the supported
interpreter subset. Allocation/dimension caps and numeric-kernel work limits
remain, without imposing a statement budget on generated native programs.

```rust
use ndarray::ArrayD;
use unlinked_matlab_rt as rt;

fn product() -> rt::Result<ArrayD<f64>> {
    let a: ArrayD<f64> = rt::array(2, 2, vec![1.0, 3.0, 2.0, 4.0])?;
    let identity: ArrayD<f64> = rt::call("eye", &[&2.0])?;
    rt::mtimes(&a, &identity)
}
```

`call::<T>` selects a result type statically; arguments use borrowed `Matlab`
trait objects solely at helper call boundaries. `convert::<T>` checks scalar
shape or converts array element kinds. `ArrayD<u8>` preserves character matrices;
`String` accepts only ASCII character rows. `assign` changes its target only if
the complete assignment succeeds. Errors are `Result<T, String>`.
