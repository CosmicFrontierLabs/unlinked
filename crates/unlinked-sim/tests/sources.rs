//! Closed-form checks of the time-based sources: Pulse Generator, Signal
//! Generator, and the library-sourced Ramp, Chirp Signal and Repeating
//! Sequence. Expected values follow the MathWorks block references cited in
//! `src/lib.rs`; integrals through an Integrator check the left-limit handling
//! at edges, and rejected configurations must name the block and parameter.
use std::collections::BTreeMap;
use unlinked_model::*;
use unlinked_sim::diagnose::{diagnose, CheckMode, DiagnosticContext};
use unlinked_sim::{compile, simulate_model, Options, Trace};

fn block(id: &str, kind: &str, params: &[(&str, &str)]) -> Block {
    Block {
        id: id.into(),
        name: id.into(),
        block_type: kind.into(),
        position: Rect::default(),
        orientation: Orientation::Right,
        mirrored: false,
        ports: PortCounts::default(),
        parameters: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        mask: None,
        library_source: None,
        subsystem: None,
        style: BlockStyle::default(),
        interface: None,
    }
}
/// A block dragged from the Simulink library: a `Reference` with its
/// `SourceBlock` path and the library mask's parameter values.
fn library(id: &str, source: &str, params: &[(&str, &str)]) -> Block {
    let mut b = block(id, "Reference", params);
    b.library_source = Some(source.into());
    b
}
fn pulse(params: &[(&str, &str)]) -> Block {
    let mut all = vec![("PulseType", "Time based")];
    all.extend_from_slice(params);
    block("p", "DiscretePulseGenerator", &all)
}
fn generator(params: &[(&str, &str)]) -> Block {
    block("g", "SignalGenerator", params)
}
fn model(blocks: Vec<Block>, lines: Vec<(&str, &str)>) -> Model {
    Model {
        name: "sources".into(),
        source: SourceFormat::Mdl,
        simulink_version: None,
        config: SimConfig::default(),
        type_defaults: Default::default(),
        charts: Vec::new(),
        workspace: BTreeMap::new(),
        root: System {
            blocks,
            lines: lines
                .into_iter()
                .map(|(src, dst)| Line {
                    src: Some(Endpoint {
                        block: src.into(),
                        port: PortRef {
                            kind: PortKind::Out,
                            index: 1,
                        },
                    }),
                    dst: Some(Endpoint {
                        block: dst.into(),
                        port: PortRef {
                            kind: PortKind::In,
                            index: 1,
                        },
                    }),
                    ..Line::default()
                })
                .collect(),
            ..System::default()
        },
    }
}
fn options(stop: f64, step: f64) -> Options {
    Options {
        stop,
        step,
        ..Options::default()
    }
}
/// Simulate `source` alone, and through an Integrator (`int`).
fn run(source: Block, stop: f64, step: f64) -> Trace {
    let id = source.id.0.clone();
    let m = model(
        vec![source, block("int", "Integrator", &[])],
        vec![(&id, "int")],
    );
    simulate_model(&m, &options(stop, step)).unwrap()
}
fn check(trace: &Trace, signal: &str, expected: impl Fn(f64) -> f64, tolerance: f64) {
    for (&t, &actual) in trace.time.iter().zip(&trace.signals[signal]) {
        let wanted = expected(t);
        assert!(
            (actual - wanted).abs() <= tolerance,
            "{signal} at t={t}: {actual} != {wanted}"
        );
    }
}
fn at(trace: &Trace, signal: &str, t: f64) -> f64 {
    let i = trace
        .time
        .iter()
        .position(|&x| (x - t).abs() < 1e-9)
        .unwrap_or_else(|| panic!("no sample at {t}"));
    trace.signals[signal][i]
}
fn error_of(source: Block, options: &Options) -> String {
    compile(&model(vec![source], vec![]), options)
        .expect_err("expected a rejection")
        .to_string()
}
fn frac(x: f64) -> f64 {
    x - x.floor()
}

#[test]
fn pulse_generator_is_zero_until_the_phase_delay_then_high_for_the_width() {
    // Period 4 s, 75 % duty (3 s high), 1 s phase delay: the fixed-step
    // example from the Pulse Generator reference.
    let trace = run(
        pulse(&[
            ("Amplitude", "2"),
            ("Period", "4"),
            ("PulseWidth", "75"),
            ("PhaseDelay", "1"),
        ]),
        9.0,
        0.25,
    );
    check(
        &trace,
        "p",
        |t| {
            if t < 1.0 || frac((t - 1.0) / 4.0) * 4.0 >= 3.0 {
                0.0
            } else {
                2.0
            }
        },
        0.0,
    );
    assert_eq!(
        at(&trace, "p", 1.0),
        2.0,
        "rising edge takes the high value"
    );
    assert_eq!(
        at(&trace, "p", 4.0),
        0.0,
        "falling edge takes the low value"
    );
    assert_eq!(at(&trace, "p", 5.0), 2.0);
    // Two complete 3 s pulses of height 2 lie in [0, 9]: the integral is exact
    // only if the solver uses the left limit at every edge.
    assert!((at(&trace, "int", 9.0) - 12.0).abs() < 1e-9);
    assert!((at(&trace, "int", 2.0) - 2.0).abs() < 1e-9);
}

#[test]
fn pulse_generator_vector_parameters_expand_elementwise() {
    let trace = run(
        pulse(&[
            ("Amplitude", "[1 2]"),
            ("Period", "4"),
            ("PulseWidth", "[25 50]"),
        ]),
        8.0,
        0.5,
    );
    check(
        &trace,
        "p[1]",
        |t| if frac(t / 4.0) * 4.0 < 1.0 { 1.0 } else { 0.0 },
        0.0,
    );
    check(
        &trace,
        "p[2]",
        |t| if frac(t / 4.0) * 4.0 < 2.0 { 2.0 } else { 0.0 },
        0.0,
    );
}

#[test]
fn pulse_generator_rejects_other_modes_and_misaligned_or_constant_pulses() {
    let o = options(10.0, 0.25);
    let sample_based = block(
        "p",
        "DiscretePulseGenerator",
        &[("PulseType", "Sample based"), ("Period", "2")],
    );
    assert!(error_of(sample_based, &o).contains("PulseType=Sample based"));
    let unspecified = block("p", "DiscretePulseGenerator", &[("Period", "2")]);
    assert!(error_of(unspecified, &o).contains("PulseType is unspecified"));
    assert!(error_of(pulse(&[("TimeSource", "Use external signal")]), &o).contains("TimeSource"));
    assert!(error_of(pulse(&[("PulseWidth", "0")]), &o).contains("PulseWidth"));
    assert!(error_of(pulse(&[("PulseWidth", "100")]), &o).contains("PulseWidth"));
    assert!(error_of(pulse(&[("PulseWidth", "120")]), &o).contains("PulseWidth"));
    assert!(error_of(pulse(&[("Period", "0")]), &o).contains("Period"));
    assert!(error_of(pulse(&[("PhaseDelay", "-1")]), &o).contains("PhaseDelay"));
    // Period 4, width 3, delay 1 fit a 0.25 s step; a 0.3 s delay does not.
    let misaligned = pulse(&[("Period", "4"), ("PulseWidth", "75"), ("PhaseDelay", "0.3")]);
    assert!(error_of(misaligned, &o).contains("align"));
    let misaligned_width = pulse(&[("Period", "4"), ("PulseWidth", "80")]);
    assert!(error_of(misaligned_width, &o).contains("align"));
    assert!(compile(
        &model(
            vec![pulse(&[
                ("Period", "4"),
                ("PulseWidth", "75"),
                ("PhaseDelay", "1")
            ])],
            vec![]
        ),
        &o
    )
    .is_ok());
}

#[test]
fn signal_generator_sine_in_both_units() {
    let rad = run(
        generator(&[("Amplitude", "3"), ("Frequency", "2")]),
        4.0,
        0.01,
    );
    check(&rad, "g", |t| 3.0 * (2.0 * t).sin(), 1e-12);
    let hertz = run(
        generator(&[("Frequency", "0.5"), ("Units", "Hertz")]),
        4.0,
        0.01,
    );
    check(&hertz, "g", |t| (std::f64::consts::PI * t).sin(), 1e-12);
    // Smooth sources need no grid alignment: 1 rad/s has a period of 2π.
    let default = run(generator(&[]), 7.0, 0.01);
    check(&default, "g", f64::sin, 1e-12);
    assert!((at(&default, "int", 7.0) - (1.0 - 7.0_f64.cos())).abs() < 1e-8);
}

#[test]
fn signal_generator_square_is_low_then_high_over_each_period() {
    // 0.5 Hz: a 2 s period whose half period is 10 steps of 0.1 s.
    let trace = run(
        generator(&[
            ("WaveForm", "square"),
            ("Amplitude", "2"),
            ("Frequency", "0.5"),
            ("Units", "Hertz"),
        ]),
        6.0,
        0.1,
    );
    check(
        &trace,
        "g",
        |t| if frac(t / 2.0) < 0.5 { -2.0 } else { 2.0 },
        0.0,
    );
    assert_eq!(at(&trace, "g", 0.0), -2.0);
    assert_eq!(at(&trace, "g", 1.0), 2.0);
    assert_eq!(at(&trace, "g", 2.0), -2.0);
    // Each period integrates to zero; the first half to -2.
    assert!((at(&trace, "int", 1.0) + 2.0).abs() < 1e-9);
    assert!(at(&trace, "int", 6.0).abs() < 1e-9);
    // π rad/s is also a 2 s period.
    let rad = run(
        generator(&[("WaveForm", "square"), ("Frequency", "pi")]),
        6.0,
        0.1,
    );
    check(
        &rad,
        "g",
        |t| if frac(t / 2.0) < 0.5 { -1.0 } else { 1.0 },
        0.0,
    );
}

#[test]
fn signal_generator_sawtooth_falls_from_plus_to_minus_amplitude() {
    let trace = run(
        generator(&[
            ("WaveForm", "sawtooth"),
            ("Amplitude", "[1 3]"),
            ("Frequency", "0.5"),
            ("Units", "Hertz"),
        ]),
        6.0,
        0.1,
    );
    check(&trace, "g[1]", |t| 1.0 - 2.0 * frac(t / 2.0), 1e-12);
    check(&trace, "g[2]", |t| 3.0 * (1.0 - 2.0 * frac(t / 2.0)), 1e-12);
    assert_eq!(at(&trace, "g[1]", 0.0), 1.0);
    assert_eq!(at(&trace, "g[1]", 2.0), 1.0, "a wrap restarts at +A");
    assert!((at(&trace, "g[1]", 1.9) + 0.9).abs() < 1e-12);
    // ∫₀¹ (1 - t) dt = 0.5 and a whole period integrates to zero, which holds
    // only if the wrap uses the left limit -A.
    assert!((at(&trace, "int[2]", 1.0) - 1.5).abs() < 1e-9);
    let single = run(
        generator(&[
            ("WaveForm", "sawtooth"),
            ("Frequency", "0.5"),
            ("Units", "Hertz"),
        ]),
        6.0,
        0.1,
    );
    assert!((at(&single, "int", 1.0) - 0.5).abs() < 1e-9);
    assert!(at(&single, "int", 6.0).abs() < 1e-9);
}

#[test]
fn signal_generator_rejects_random_other_units_and_misaligned_edges() {
    let o = options(6.0, 0.1);
    assert!(error_of(generator(&[("WaveForm", "random")]), &o).contains("WaveForm=random"));
    assert!(error_of(generator(&[("Units", "kHz")]), &o).contains("Units"));
    assert!(
        error_of(generator(&[("TimeSource", "Use external signal")]), &o).contains("TimeSource")
    );
    // A 2π s period cannot land on a decimal grid.
    assert!(error_of(generator(&[("WaveForm", "square")]), &o).contains("align"));
    assert!(error_of(generator(&[("WaveForm", "sawtooth")]), &o).contains("align"));
    let negative = generator(&[
        ("WaveForm", "square"),
        ("Frequency", "-0.5"),
        ("Units", "Hertz"),
    ]);
    assert!(error_of(negative, &o).contains("Frequency"));
}

#[test]
fn ramp_holds_its_initial_output_until_the_start_time() {
    let ramp = library(
        "r",
        "simulink/Sources/Ramp",
        &[("slope", "2"), ("start", "1"), ("X0", "0.5")],
    );
    let trace = run(ramp, 4.0, 0.01);
    check(
        &trace,
        "r",
        |t| if t < 1.0 { 0.5 } else { 0.5 + 2.0 * (t - 1.0) },
        1e-12,
    );
    // ∫₀⁴ = 0.5·4 + 2·3²/2 = 11.
    assert!((at(&trace, "int", 4.0) - 11.0).abs() < 1e-9);
    let vector = run(
        library(
            "r",
            "simulink/Sources/Ramp",
            &[("slope", "[1 -1]"), ("InitialOutput", "[0 1]")],
        ),
        2.0,
        0.1,
    );
    check(&vector, "r[1]", |t| t, 1e-12);
    check(&vector, "r[2]", |t| 1.0 - t, 1e-12);
}

#[test]
fn chirp_sweeps_linearly_and_keeps_sweeping_past_the_target_time() {
    // 0.5 Hz to 1.5 Hz over 2 s: phase 2π(0.5 t + 0.25 t²).
    let chirp = library(
        "c",
        "simulink/Sources/Chirp Signal",
        &[("f1", "0.5"), ("T", "2"), ("f2", "1.5")],
    );
    let trace = run(chirp, 4.0, 0.01);
    let expected = |t: f64| (std::f64::consts::TAU * (0.5 * t + 0.25 * t * t)).sin();
    check(&trace, "c", expected, 1e-9);
    // After T the instantaneous frequency is still f1 + (f2 - f1) t / T.
    assert!(trace.time.iter().any(|&t| t > 2.0));
    let phased = library(
        "c",
        "simulink/Sources/Chirp Signal",
        &[
            ("f1", "[0.5 1]"),
            ("T", "2"),
            ("f2", "1.5"),
            ("phi", "pi/2"),
        ],
    );
    let trace = run(phased, 4.0, 0.01);
    check(
        &trace,
        "c[1]",
        |t| (std::f64::consts::TAU * (0.5 * t + 0.25 * t * t)).cos(),
        1e-9,
    );
    check(
        &trace,
        "c[2]",
        |t| (std::f64::consts::TAU * (t + 0.125 * t * t)).cos(),
        1e-9,
    );
    let late = Options {
        start: 1.0,
        ..options(4.0, 0.01)
    };
    assert!(
        error_of(library("c", "simulink/Sources/Chirp Signal", &[]), &late).contains("start time")
    );
    assert!(error_of(
        library("c", "simulink/Sources/Chirp Signal", &[("T", "0")]),
        &options(4.0, 0.01)
    )
    .contains("T (target time)"));
}

#[test]
fn repeating_sequence_interpolates_and_repeats_with_the_last_time_as_period() {
    let sequence = library(
        "s",
        "simulink/Sources/Repeating\nSequence",
        &[("rep_seq_t", "[0 1 3]"), ("rep_seq_y", "[0 2 -1]")],
    );
    let trace = run(sequence, 7.0, 0.1);
    check(
        &trace,
        "s",
        |t| {
            let u = frac(t / 3.0) * 3.0;
            if u < 1.0 {
                2.0 * u
            } else {
                2.0 - 1.5 * (u - 1.0)
            }
        },
        1e-9,
    );
    assert_eq!(
        at(&trace, "s", 3.0),
        0.0,
        "the sequence restarts at n*period"
    );
    assert!((at(&trace, "s", 2.0) - 0.5).abs() < 1e-12);
    // One period integrates to 1 + 1 = 2 (triangle up, then down through -1).
    assert!((at(&trace, "int", 3.0) - (1.0 + 2.0 * 0.5 * 2.0 - 1.0)).abs() < 1e-9);
    // The default [0 2] -> [0 2] wraps discontinuously; its left limit is 2.
    let default = run(
        library("s", "simulink/Sources/Repeating\nSequence", &[]),
        4.0,
        0.1,
    );
    check(&default, "s", |t| frac(t / 2.0) * 2.0, 1e-9);
    assert!((at(&default, "int", 4.0) - 4.0).abs() < 1e-9);
}

#[test]
fn repeating_sequence_rejects_bad_tables_and_misaligned_jumps() {
    let o = options(4.0, 0.1);
    let seq = |t: &str, y: &str| {
        library(
            "s",
            "simulink/Sources/Repeating Sequence",
            &[("rep_seq_t", t), ("rep_seq_y", y)],
        )
    };
    assert!(error_of(seq("[1 2]", "[0 1]"), &o).contains("start at 0"));
    assert!(error_of(seq("[0 2 2]", "[0 1 0]"), &o).contains("strictly increasing"));
    assert!(error_of(seq("[0 2]", "[0 1 0]"), &o).contains("equal length"));
    assert!(error_of(seq("[0]", "[1]"), &o).contains("2 to 1024"));
    // A 0.35 s period of a discontinuous waveform misses the 0.1 s grid ...
    assert!(error_of(seq("[0 0.35]", "[0 1]"), &o).contains("align"));
    // ... but a continuous wrap has no edge to align.
    assert!(compile(&model(vec![seq("[0 0.35]", "[1 1]")], vec![]), &o).is_ok());
}

#[test]
fn library_registry_matches_the_catalog_and_other_links_still_reject() {
    for (source, key) in [
        ("simulink/Sources/Ramp", "Ramp"),
        ("simulink/Sources/Chirp Signal", "ChirpSignal"),
        ("simulink/Sources/Repeating\nSequence", "RepeatingSequence"),
    ] {
        let descriptor = catalog::find_by_source(source).unwrap();
        assert_eq!(descriptor.type_key, key);
        assert!(descriptor.creatable);
        let m = model(vec![library("b", source, &[])], vec![]);
        compile(&m, &options(2.0, 0.01)).unwrap_or_else(|e| panic!("{source}: {e}"));
        let report = diagnose(&m, &DiagnosticContext::default());
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == "simulation_link_or_mask"),
            "{source} is not an unsupported link"
        );
    }
    let noise = library("n", "simulink/Sources/Band-Limited White Noise", &[]);
    let m = model(vec![noise], vec![]);
    assert!(error_of(m.root.blocks[0].clone(), &options(2.0, 0.01)).contains("library links"));
    let report = diagnose(
        &m,
        &DiagnosticContext {
            mode: CheckMode::Static,
            ..Default::default()
        },
    );
    assert!(report
        .diagnostics
        .iter()
        .any(|d| d.code == "simulation_link_or_mask"));
    // A Reference block with its own mask is not resolved through the registry.
    let mut masked = library("m", "simulink/Sources/Ramp", &[]);
    masked.mask = Some(Mask::default());
    assert!(error_of(masked, &options(2.0, 0.01)).contains("masks"));
}
