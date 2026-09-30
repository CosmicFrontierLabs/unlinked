mod coverage;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use unlinked_model::Model;
use unlinked_sim::{Options, Solver, Trace};

const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "unlinked",
    version,
    about = "Inspect, render and simulate Simulink files; transpile scalar MATLAB to Rust/LLVM"
)]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Inventory import, rendering and simulation compilation for a model directory.
    Coverage {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import an SLX or MDL model and print structured JSON statistics.
    Info {
        /// Model filename, or - for stdin (format is detected from content).
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Render the root diagram or a nested subsystem as standalone SVG.
    Render {
        input: PathBuf,
        /// Exact subsystem block name; repeat for each level (slashes are literal).
        #[arg(long = "system", value_name = "BLOCK_NAME")]
        system: Vec<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run the supported scalar simulation subset with explicitly selected settings.
    Sim {
        input: PathBuf,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        start: f64,
        #[arg(long, allow_hyphen_values = true)]
        stop: f64,
        #[arg(long, allow_hyphen_values = true)]
        step: f64,
        #[arg(long, value_enum)]
        solver: SolverArg,
        #[arg(long, default_value_t = 100_001)]
        max_samples: usize,
        /// Relative local-error target for adaptive RK45.
        #[arg(long, default_value_t = 1e-6)]
        rtol: f64,
        /// Absolute local-error target for adaptive RK45.
        #[arg(long, default_value_t = 1e-9)]
        atol: f64,
        #[arg(long, default_value_t = 100_000)]
        max_internal_steps: usize,
        /// Add a scalar workspace expression (repeatable); never execute a MATLAB script.
        #[arg(long = "var", value_name = "NAME=EXPR")]
        variables: Vec<String>,
        /// JSON or CSV; defaults to CSV for .csv output paths, otherwise JSON.
        #[arg(long, value_enum)]
        format: Option<TraceFormat>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Translate a scalar MATLAB/Octave script or function file. Never execute it.
    Transpile {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Export functions as pub fn f_name; reject script statements.
        #[arg(long)]
        library: bool,
        /// LLVM output invokes local rustc to compile generated Rust, without running it.
        #[arg(long, value_enum, default_value_t = Emit::Rust)]
        emit: Emit,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum SolverArg {
    Euler,
    Rk4,
    Rk45,
}
#[derive(Clone, Copy, ValueEnum)]
enum TraceFormat {
    Json,
    Csv,
}
#[derive(Clone, Copy, ValueEnum)]
enum Emit {
    Rust,
    LlvmIr,
}
impl From<SolverArg> for Solver {
    fn from(value: SolverArg) -> Self {
        match value {
            SolverArg::Euler => Self::Euler,
            SolverArg::Rk4 => Self::Rk4,
            SolverArg::Rk45 => Self::Rk45,
        }
    }
}

fn read_input(path: &Path) -> Result<Vec<u8>> {
    let reader: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).with_context(|| format!("cannot open {}", path.display()))?)
    };
    let mut bytes = Vec::new();
    reader
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        bail!("input exceeds the 16 MiB limit");
    }
    Ok(bytes)
}
fn load_model(path: &Path) -> Result<Model> {
    let bytes = read_input(path)?;
    unlinked_import::import(&path.to_string_lossy(), &bytes)
        .with_context(|| format!("cannot import {}", path.display()))
}
fn write_output(path: Option<&Path>, bytes: &[u8]) -> Result<()> {
    if let Some(path) = path.filter(|p| *p != Path::new("-")) {
        std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    } else {
        io::stdout()
            .lock()
            .write_all(bytes)
            .context("cannot write stdout")?;
    }
    Ok(())
}
fn model_info(model: &Model) -> serde_json::Value {
    let systems = model.walk();
    let summaries: Vec<_> = systems
        .iter()
        .map(|(path, system)| {
            serde_json::json!({
                "path": path, "blocks": system.blocks.len(), "lines": system.lines.len(),
                "connections": system.connections().len(), "annotations": system.annotations.len(),
            })
        })
        .collect();
    let masked_blocks = systems
        .iter()
        .flat_map(|(_, s)| &s.blocks)
        .filter(|b| b.mask.is_some())
        .count();
    let library_links = systems
        .iter()
        .flat_map(|(_, s)| &s.blocks)
        .filter(|b| b.library_source.is_some())
        .count();
    serde_json::json!({
        "name": model.name, "source_format": model.source, "simulink_version": model.simulink_version,
        "blocks": model.block_count(), "block_types": model.block_type_counts(),
        "systems": summaries, "masked_blocks": masked_blocks, "library_links": library_links,
        "workspace_variables": model.workspace.keys().collect::<Vec<_>>(), "imported_config": model.config,
    })
}
fn inject_workspace(model: &mut Model, variables: &[String]) -> Result<()> {
    const KEYWORDS: &[&str] = &[
        "break",
        "case",
        "catch",
        "classdef",
        "continue",
        "else",
        "elseif",
        "end",
        "for",
        "function",
        "global",
        "if",
        "otherwise",
        "parfor",
        "persistent",
        "return",
        "spmd",
        "switch",
        "try",
        "while",
    ];
    for definition in variables {
        let (name, expression) = definition
            .split_once('=')
            .context("workspace variable must have form NAME=EXPR")?;
        let name = name.trim();
        let expression = expression.trim();
        if name.len() > 63
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || KEYWORDS.contains(&name)
        {
            bail!(
                "invalid workspace identifier {name:?}; use up to 63 ASCII letters/digits/underscores, beginning with a letter, excluding MATLAB keywords"
            );
        }
        if expression.is_empty() {
            bail!("workspace expression for '{name}' is empty");
        }
        if model.workspace.contains_key(name) {
            bail!("duplicate workspace variable '{name}'");
        }
        model.workspace.insert(name.into(), expression.into());
    }
    Ok(())
}
fn trace_csv(trace: &Trace) -> Result<Vec<u8>> {
    let mut csv = csv::Writer::from_writer(Vec::new());
    let header = std::iter::once("time").chain(trace.signals.keys().map(String::as_str));
    csv.write_record(header)?;
    for (index, time) in trace.time.iter().enumerate() {
        let row = std::iter::once(time.to_string())
            .chain(trace.signals.values().map(|v| v[index].to_string()));
        csv.write_record(row)?;
    }
    csv.into_inner().map_err(|e| e.into_error().into())
}
fn compile_llvm(source: &str, library: bool, output: &Path) -> Result<()> {
    // Only our generated Rust is sent to rustc. There are no external crates,
    // build scripts, proc macros, user-supplied Rust, or executable invocation.
    let mut child = Command::new("rustc")
        .args([
            "--edition=2024",
            "--crate-name",
            "unlinked_generated",
            "--crate-type",
            if library { "lib" } else { "bin" },
            "--emit=llvm-ir",
            "-",
        ])
        .arg("-o")
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start rustc; install Rust or use --emit rust")?;
    let write = child
        .stdin
        .take()
        .context("rustc stdin unavailable")?
        .write_all(source.as_bytes());
    let result = child.wait_with_output().context("cannot wait for rustc")?;
    if !result.status.success() {
        bail!("rustc failed: {}", String::from_utf8_lossy(&result.stderr));
    }
    write.context("cannot send generated Rust to compiler")?;
    Ok(())
}
fn run(args: Args) -> Result<()> {
    match args.command {
        Action::Coverage { input, output } => {
            let mut json = serde_json::to_vec_pretty(&coverage::report(&input)?)?;
            json.push(b'\n');
            write_output(output.as_deref(), &json)
        }
        Action::Info { input, output } => {
            let model = load_model(&input)?;
            let mut json = serde_json::to_vec_pretty(&model_info(&model))?;
            json.push(b'\n');
            write_output(output.as_deref(), &json)
        }
        Action::Render {
            input,
            system,
            output,
        } => {
            let model = load_model(&input)?;
            let path: Vec<&str> = system.iter().map(String::as_str).collect();
            let options = unlinked_render::RenderOptions {
                theme: unlinked_render::Theme::Dark,
                ..Default::default()
            };
            let svg =
                unlinked_render::render_svg(&model, &path, &options).context("rendering failed")?;
            write_output(output.as_deref(), svg.as_bytes())
        }
        Action::Sim {
            input,
            start,
            stop,
            step,
            solver,
            max_samples,
            rtol,
            atol,
            max_internal_steps,
            variables,
            format,
            output,
        } => {
            let mut model = load_model(&input)?;
            inject_workspace(&mut model, &variables)?;
            let options = Options {
                start,
                stop,
                step,
                solver: solver.into(),
                max_samples,
                relative_tolerance: rtol,
                absolute_tolerance: atol,
                max_internal_steps,
            };
            eprintln!(
                "Simulating {}: {:?}, start={start}, stop={stop}, step={step}; these explicit settings override imported solver settings",
                model.name, options.solver
            );
            let trace =
                unlinked_sim::simulate_model(&model, &options).context("simulation failed")?;
            let format = format.unwrap_or_else(|| {
                if output
                    .as_ref()
                    .and_then(|p| p.extension())
                    .is_some_and(|s| s.eq_ignore_ascii_case("csv"))
                {
                    TraceFormat::Csv
                } else {
                    TraceFormat::Json
                }
            });
            let bytes = match format {
                TraceFormat::Csv => trace_csv(&trace)?,
                TraceFormat::Json => {
                    let mut json = serde_json::to_vec_pretty(
                        &serde_json::json!({"model":model.name,"options":options,"imported_config":model.config,"workspace":model.workspace,"trace":trace}),
                    )?;
                    json.push(b'\n');
                    json
                }
            };
            write_output(output.as_deref(), &bytes)
        }
        Action::Transpile {
            input,
            output,
            library,
            emit,
        } => {
            let bytes = read_input(&input)?;
            let source = std::str::from_utf8(&bytes).context("MATLAB source must be UTF-8")?;
            let generated = if library {
                unlinked_matlab::transpile_library(source)
            } else {
                unlinked_matlab::transpile(source)
            }
            .context("transpilation failed")?;
            match emit {
                Emit::Rust => write_output(output.as_deref(), generated.as_bytes()),
                Emit::LlvmIr => {
                    let output = output.as_deref().filter(|p| *p != Path::new("-")).context(
                        "--emit llvm-ir requires --output PATH (LLVM output cannot be stdout)",
                    )?;
                    compile_llvm(&generated, library, output)
                }
            }
        }
    }
}
fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
