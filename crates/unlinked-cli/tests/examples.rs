//! Every model in `examples/` imports, renders and simulates, and its
//! response shows the behaviour the example describes.
//!
//! These are numerical behaviour checks of the designs (settling, limits,
//! sampling), not validation against physical hardware. The expected values
//! were cross-checked with an independent SciPy integration of the same
//! equations.

use std::path::{Path, PathBuf};
use unlinked_model::Model;
use unlinked_sim::{Options, Solver, Trace};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn load(name: &str) -> Model {
    let path = examples_dir().join(name);
    let bytes = std::fs::read(&path).unwrap();
    unlinked_import::import(name, &bytes).unwrap()
}

/// Apply an init script the way the server does for numeric scalars.
fn apply_init(model: &mut Model, script: &str) {
    let source = std::fs::read_to_string(examples_dir().join(script)).unwrap();
    let values = unlinked_matlab::eval_script(&source, &Default::default()).unwrap();
    for (name, value) in values {
        assert_eq!(value.data.len(), 1, "{name} is scalar");
        model.workspace.insert(name, value.data[0].to_string());
    }
}

fn run(model: &Model, stop: f64, step: f64) -> Trace {
    let options = Options {
        stop,
        step,
        solver: Solver::Rk4,
        ..Options::default()
    };
    unlinked_sim::simulate_model(model, &options).unwrap()
}

/// Samples of the signal with block id `id` at time `t`.
fn at(trace: &Trace, id: &str, t: f64) -> f64 {
    let i = trace
        .time
        .iter()
        .position(|&x| (x - t).abs() < 1e-9)
        .unwrap();
    trace.signals[id][i]
}

fn max_abs(trace: &Trace, id: &str) -> f64 {
    trace.signals[id].iter().fold(0.0, |m, v| m.max(v.abs()))
}

fn close(actual: f64, expected: f64, tol: f64) {
    assert!(
        (actual - expected).abs() <= tol,
        "expected {expected} ± {tol}, got {actual}"
    );
}

#[test]
fn every_example_imports_and_renders() {
    let mut count = 0;
    for entry in std::fs::read_dir(examples_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mdl") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let model = load(&name);
        unlinked_render::render_svg(&model, &[], &Default::default()).unwrap();
        count += 1;
    }
    assert_eq!(count, 6);
}

#[test]
fn cart_pole_lqr_rebalances() {
    let t = run(&load("inverted_pendulum_lqr.mdl"), 5.0, 0.01);
    // Pendulum angle (id 9) from 0.2 rad back to upright; cart (7) returns.
    close(at(&t, "9", 0.0), 0.2, 1e-12);
    close(at(&t, "9", 1.0), 0.0151, 1e-3);
    close(at(&t, "9", 5.0), 0.0, 1e-4);
    close(at(&t, "7", 5.0), 0.0, 1e-4);
    close(max_abs(&t, "7"), 0.1525, 1e-3);
    // Initial force is -K x0.
    close(at(&t, "11", 0.0), -21.106, 1e-2);
}

#[test]
fn nonlinear_pendulum_recovers_under_torque_limit() {
    let t = run(&load("inverted_pendulum_nonlinear_pd.mdl"), 4.0, 0.005);
    close(at(&t, "12", 0.5), 0.2376, 1e-3);
    close(at(&t, "12", 4.0), 0.0, 1e-4);
    // The controller starts saturated at the 3 N m limit.
    close(at(&t, "13", 0.0), -3.0, 1e-12);
    assert!(max_abs(&t, "13") <= 3.0 + 1e-12);
}

#[test]
fn mass_spring_damper_pid_tracks_step_with_init_script() {
    let mut model = load("mass_spring_damper_pid.mdl");
    apply_init(&mut model, "mass_spring_damper_params.m");
    let t = run(&model, 3.0, 0.001);
    close(at(&t, "9", 0.5), 0.0, 1e-12);
    close(at(&t, "9", 1.0), 0.9714, 1e-3);
    close(at(&t, "9", 3.0), 0.996, 1e-3);
}

#[test]
fn dc_motor_pi_settles_within_supply_limit() {
    let t = run(&load("dc_motor_speed_pi.mdl"), 3.0, 0.001);
    let peak = t.signals["9"].iter().cloned().fold(f64::MIN, f64::max);
    close(peak, 1.0743, 1e-3);
    close(at(&t, "9", 3.0), 1.0, 1e-3);
    assert!(max_abs(&t, "10") <= 24.0 + 1e-12);
}

#[test]
fn cruise_control_rejects_hill() {
    let t = run(&load("cruise_control_pi.mdl"), 30.0, 0.01);
    close(at(&t, "11", 1.0), 0.0, 1e-12);
    let dip = t.signals["11"][1500..]
        .iter()
        .cloned()
        .fold(f64::MAX, f64::min);
    close(dip, 9.691, 5e-3);
    close(at(&t, "11", 30.0), 9.981, 5e-3);
    assert!(max_abs(&t, "12") <= 5000.0 + 1e-9);
}

#[test]
fn digital_pi_updates_only_at_sample_hits() {
    let t = run(&load("digital_pi_first_order.mdl"), 10.0, 0.01);
    let u = &t.signals["13"];
    for i in 1..u.len() {
        if i % 10 != 0 {
            assert_eq!(u[i], u[i - 1], "control changed between samples at {i}");
        }
    }
    // Seeded noise: settles near the setpoint.
    let tail = &t.signals["12"][800..];
    close(tail.iter().sum::<f64>() / tail.len() as f64, 1.0, 0.02);
}
