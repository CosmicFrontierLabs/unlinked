# Example control systems

Hand-written Simulink models (MDL) that run in Unlinked's simulator. Each
uses only supported blocks, and `crates/unlinked-cli/tests/examples.rs`
checks that it imports, renders, simulates and shows the behaviour below.
Those are numerical checks of the designs, cross-checked against an
independent SciPy integration of the same equations; they are not
validation against physical hardware.

| Model | What it shows | Run with |
|---|---|---|
| `inverted_pendulum_lqr.mdl` | Linearized cart-pole (M = 0.5 kg, m = 0.2 kg, l = 0.3 m, I = 0.006 kg m², b = 0.1 N s/m) with LQR state feedback, built from vector signals and matrix gains. Starts tilted 0.2 rad and returns upright within about 3 s; the cart moves up to about 0.153 m. | RK4, step 0.01 s, stop 5 s |
| `inverted_pendulum_nonlinear_pd.mdl` | Nonlinear pendulum (sin term) balanced upright by a PD controller with a 3 N m torque limit, starting 0.8 rad from upright. | RK4, step 0.005 s, stop 4 s |
| `mass_spring_damper_pid.mdl` | Position control with a PID controller (filtered derivative). Parameters come from `mass_spring_damper_params.m`, selected as the init script. | RK4, step 0.001 s, stop 3 s, init script |
| `dc_motor_speed_pi.mdl` | DC motor speed loop, PI with a 24 V supply limit; about 7 % overshoot. | RK4, step 0.001 s, stop 3 s |
| `cruise_control_pi.mdl` | Vehicle speed PI controller with engine force limits; a hill at t = 15 s is rejected by integral action. | RK4, step 0.01 s, stop 30 s |
| `digital_pi_first_order.mdl` | PI controller sampled every 0.1 s (zero-order hold, unit-delay integrator) with seeded sensor noise. | RK4, step 0.01 s, stop 10 s |

Run one from the command line:

```sh
cargo run -p unlinked-cli -- sim examples/cruise_control_pi.mdl --stop 30 --step 0.01 --solver rk4 -o cruise.csv
```

The mass-spring-damper needs its parameters; on the command line pass them
with `--var m=1 --var b=10 ...`, in the web app choose the init script on the
Simulate tab.

## Loading them into a server

`scripts/seed-demo.py` creates an "Unlinked Examples" organization with a
project per topic, uploads everything and runs each model once with the
settings above (init script versions pinned), so a demo opens with results:

```sh
scripts/seed-demo.py --url http://localhost:3000
```

It signs in with dev login, so the server must run with `--dev-mode`, or pass
`--session` with the value of a signed-in `unlinked_session` cookie.
