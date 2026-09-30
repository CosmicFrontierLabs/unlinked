//! Imports every model in the unlinked-test-cases corpus.
//!
//! The corpus location comes from `UNLINKED_TEST_CASES`, defaulting to a
//! sibling checkout of the workspace. The test is skipped when absent.

use std::path::{Path, PathBuf};
use unlinked_model::scope::ScopeConfig;
use unlinked_model::stateflow::split_path;
use unlinked_model::{ChartKind, DataScope, Model, System};

fn corpus_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("UNLINKED_TEST_CASES") {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../unlinked-test-cases"),
    };
    dir.is_dir().then_some(dir)
}

fn models(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            models(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("slx" | "mdl")) {
            out.push(p);
        }
    }
}

/// Every line endpoint must name a block in the same system with a port
/// that exists after import.
fn check_system(path: &str, sys: &System, problems: &mut Vec<String>) {
    for c in sys.connections() {
        for ep in [&c.src, &c.dst] {
            match sys.block(&ep.block) {
                None => problems.push(format!(
                    "{path}: line references missing block {}",
                    ep.block
                )),
                Some(b) if b.ports.count(ep.port.kind) < ep.port.index => {
                    problems.push(format!("{path}: {} has no port {:?}", b.name, ep.port))
                }
                _ => {}
            }
        }
    }
    for b in &sys.blocks {
        if let Some(sub) = &b.subsystem {
            check_system(&format!("{path}/{}", b.name), sub, problems);
        }
    }
}

#[test]
fn corpus_models_import() {
    let Some(dir) = corpus_dir() else {
        eprintln!("corpus not found; set UNLINKED_TEST_CASES to run");
        return;
    };
    let mut files = Vec::new();
    models(&dir, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no models under {}", dir.display());

    let mut failures = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&dir).unwrap().display().to_string();
        let bytes = std::fs::read(f).unwrap();
        match unlinked_import::import(&rel, &bytes) {
            Ok(model) => {
                let mut problems = Vec::new();
                check_system(&model.name, &model.root, &mut problems);
                eprintln!(
                    "ok   {rel}: {} blocks, {} systems, version {:?}{}",
                    model.block_count(),
                    model.walk().len(),
                    model.simulink_version,
                    if problems.is_empty() {
                        String::new()
                    } else {
                        format!(", {} problems", problems.len())
                    }
                );
                failures.extend(problems.into_iter().map(|p| format!("{rel}: {p}")));
            }
            Err(e) => {
                eprintln!("FAIL {rel}: {e}");
                failures.push(format!("{rel}: {e}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every Stateflow-backed block must have its chart, and every chart must
/// name a block.
fn check_charts(model: &Model, problems: &mut Vec<String>) {
    fn blocks(sys: &System, path: &mut Vec<String>, model: &Model, problems: &mut Vec<String>) {
        for b in &sys.blocks {
            path.push(b.name.clone());
            let refs: Vec<&str> = path.iter().map(String::as_str).collect();
            match (b.stateflow_type(), model.chart_at(&refs)) {
                (Some(_), None) => problems.push(format!("no chart for {}", path.join("/"))),
                (_, Some(c)) if c.kind == ChartKind::MatlabFunction => {
                    let count = |scope| c.data.iter().filter(|d| d.scope == scope).count() as u32;
                    let ports = (count(DataScope::Input), count(DataScope::Output));
                    if ports != (b.ports.inputs, b.ports.outputs) {
                        problems.push(format!(
                            "{}: chart data {ports:?} vs block ports {:?}",
                            path.join("/"),
                            (b.ports.inputs, b.ports.outputs)
                        ));
                    }
                }
                _ => {}
            }
            if let Some(sub) = &b.subsystem {
                blocks(sub, path, model, problems);
            }
            path.pop();
        }
    }
    blocks(&model.root, &mut Vec::new(), model, problems);
    for c in &model.charts {
        let names = split_path(&c.name);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (parent, last) = refs.split_at(refs.len() - 1);
        if model
            .system_at(parent)
            .and_then(|s| s.block_by_name(last[0]))
            .is_none()
        {
            problems.push(format!("chart {:?} names no block", c.name));
        }
    }
}

/// Whether the file carries Stateflow data at all; some corpus files keep
/// MATLAB Function blocks but were saved without it.
fn has_stateflow(bytes: &[u8]) -> bool {
    match unlinked_import::slx::SlxPackage::open(bytes) {
        Ok(pkg) => pkg.has("simulink/stateflow.xml") || pkg.has("simulink/stateflow/machine.xml"),
        Err(_) => bytes.windows(12).any(|w| w == b"\nStateflow {"),
    }
}

#[test]
fn corpus_charts_import() {
    let Some(dir) = corpus_dir() else { return };
    let mut files = Vec::new();
    models(&dir, &mut files);
    files.sort();
    let mut problems = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&dir).unwrap().display().to_string();
        let bytes = std::fs::read(f).unwrap();
        let Ok(model) = unlinked_import::import(&rel, &bytes) else {
            continue;
        };
        if !model.charts.is_empty() {
            let states: usize = model.charts.iter().map(|c| c.states.len()).sum();
            eprintln!("{rel}: {} charts, {states} states", model.charts.len());
        }
        if has_stateflow(&bytes) {
            let mut p = Vec::new();
            check_charts(&model, &mut p);
            problems.extend(p.into_iter().map(|p| format!("{rel}: {p}")));
        }
        for (name, min) in [("rovSim_los.slx", 2), ("DISCON_NREL5MW.slx", 5)] {
            if rel.ends_with(name) {
                assert!(model.charts.len() >= min, "{rel}: {:?}", model.charts);
                assert!(model
                    .charts
                    .iter()
                    .all(|c| c.kind == ChartKind::MatlabFunction
                        && c.script
                            .as_deref()
                            .is_some_and(|s| s.starts_with("function"))));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// Count final line segments (last vertex → destination anchor) that are
/// axis-aligned. Older releases store every vertex of an orthogonal route,
/// so a high ratio confirms port placement matches Simulink's. Newer
/// releases store only routing hints and auto-route the final leg, which is
/// why the ratio is not close to 1.
fn orthogonality(sys: &System, aligned: &mut usize, total: &mut usize) {
    use unlinked_model::geometry::port_anchor;
    use unlinked_model::{Branch, Endpoint, Point, PortKind};
    fn check(
        sys: &System,
        last: Option<Point>,
        dst: &Option<Endpoint>,
        aligned: &mut usize,
        total: &mut usize,
    ) {
        let (Some(last), Some(dst)) = (last, dst) else {
            return;
        };
        let Some(b) = sys.block(&dst.block) else {
            return;
        };
        // Physical connection trees are undirected; only check signal lines.
        if matches!(dst.port.kind, PortKind::LConn | PortKind::RConn) {
            return;
        }
        let a = port_anchor(b, dst.port);
        *total += 1;
        if (a.x - last.x).abs() < 1.5 || (a.y - last.y).abs() < 1.5 {
            *aligned += 1;
        }
    }
    fn branches(
        sys: &System,
        last: Option<Point>,
        bs: &[Branch],
        aligned: &mut usize,
        total: &mut usize,
    ) {
        for b in bs {
            let tail = b.points.last().copied().or(last);
            if !b.points.is_empty() {
                check(sys, tail, &b.dst, aligned, total);
            }
            branches(sys, tail, &b.branches, aligned, total);
        }
    }
    for l in &sys.lines {
        let tail = l.points.last().copied();
        if tail.is_some() {
            check(sys, tail, &l.dst, aligned, total);
        }
        branches(sys, tail, &l.branches, aligned, total);
    }
    for b in &sys.blocks {
        if let Some(sub) = &b.subsystem {
            orthogonality(sub, aligned, total);
        }
    }
}

#[test]
fn corpus_line_routing_is_orthogonal() {
    let Some(dir) = corpus_dir() else { return };
    let mut files = Vec::new();
    models(&dir, &mut files);
    let (mut aligned, mut total) = (0, 0);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        if let Ok(model) = unlinked_import::import(&f.display().to_string(), &bytes) {
            let (mut a, mut t) = (0, 0);
            orthogonality(&model.root, &mut a, &mut t);
            eprintln!("{:5}/{:5} {}", a, t, f.display());
            aligned += a;
            total += t;
        }
    }
    let ratio = aligned as f64 / total.max(1) as f64;
    eprintln!("orthogonal final segments: {aligned}/{total} = {ratio:.3}");
    assert!(
        ratio > 0.65,
        "port geometry disagrees with stored routing: {ratio:.3}"
    );
}

/// Every Scope block's configuration is readable, and every specification
/// that saves display settings yields at least one display.
#[test]
fn corpus_scope_configs_parse() {
    let Some(dir) = corpus_dir() else { return };
    let mut files = Vec::new();
    models(&dir, &mut files);
    let mut problems = Vec::new();
    let mut specs = 0;
    for f in &files {
        let rel = f.strip_prefix(&dir).unwrap().display().to_string();
        let bytes = std::fs::read(f).unwrap();
        let Ok(model) = unlinked_import::import(&rel, &bytes) else {
            continue;
        };
        for (path, sys) in model.walk() {
            for b in &sys.blocks {
                match ScopeConfig::from_block(b) {
                    None => {}
                    Some(Err(e)) => problems.push(format!("{rel}: {path}/{}: {e}", b.name)),
                    Some(Ok(cfg)) if b.param("ScopeSpecificationString").is_some() => {
                        specs += 1;
                        let saved = b
                            .param("ScopeSpecificationString")
                            .is_some_and(|s| s.contains("SerializedDisplays"));
                        if saved && cfg.displays.is_empty() {
                            problems.push(format!("{rel}: {path}/{}: no displays", b.name));
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(specs > 0, "corpus has scope specifications");
}
