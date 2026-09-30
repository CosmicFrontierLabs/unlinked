# Compatibility evidence

The [machine-readable inventory](corpus-coverage.json) records import, rendering and compilation outcomes for the pinned public corpus at revision `7684ab11500cda4d0f1192287360d1d767dafe4c`. It is a baseline, not a general compatibility guarantee.

All 31 model fixtures import and render. With no external input bindings or workspace overrides, one model compiles for execution: the synthetic analytic reference. Existing models commonly require external inputs, workspace values, unsupported masks, libraries or blocks. The report retains each compiler diagnostic.

With explicit root input bindings, the original AutoLayout MDL and SLX files both execute and produce identical traces across 35 scalar signals. Reproduce either run with:

```sh
unlinked sim path/to/AutoLayoutDemo.mdl \
  --input-value 9=1 --input-value 24=2 --input-value 1=3 \
  --stop 0.2 --step 0.1 --solver rk4 -o trace.json
```

This checks equivalent file encodings; it is not an independent MathWorks numerical oracle. Separate solver tests compare analytic trajectories, transfer/state-space responses and discrete recurrences. The synthetic corpus model has the independent formula `y = 1 + 10*t` and is checked within `1e-9`.

All nine collected MATLAB functions compile to Rust and match Octave on the recorded test inputs. Additional differential tests cover arrays and initialization scripts. The pure function interpreter deliberately rejects printing; its tests distinguish original files from test-only projections with printing statements removed. Supported operations, bounds, and MATLAB/Octave differences are listed in the [compiler README](../crates/unlinked-matlab/README.md). The [simulator README](../crates/unlinked-sim/README.md) documents supported block and solver semantics.

Refresh the default model inventory with `unlinked coverage /path/to/unlinked-test-cases/fixtures -o coverage.json`. Keep configured-input scenarios separate: accepting missing inputs as implicit constants would obscure meaningful compatibility failures.
