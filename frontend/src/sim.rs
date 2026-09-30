//! Run simulations of a stored model version and plot the streamed traces
//! with rizzma (oscilloscope-styled, zoom/pan in the canvas).
//!
//! Streaming state lives in one [`Controller`] shared by the render function
//! and the async tasks. Every data set (a live run or a loaded historical
//! run) gets a new generation number; tasks drop results for any other
//! generation, so a stale stream or response can never overwrite newer data.

use crate::api;
use crate::fetch::{use_fetch, use_reload, view, Reload};
use crate::plot::{self, PlotSession};
use futures_util::future::{AbortHandle, Abortable};
use rizzma::wasm::WasmFigure;
use shared::{
    SimulationClientMsg, SimulationOptions, SimulationRequest, SimulationRun, SimulationServerMsg,
    SimulationSignal, SimulationSocket, SimulationStatus, Solver,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use unlinked_model::SimConfig;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Event, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement, InputEvent};
use ws_bridge::yew_client::Sender;
use yew::prelude::*;

/// Signals plotted by default when a data set loads.
const DEFAULT_PLOTTED: usize = 6;
/// Minimum time between plot refreshes while samples stream in.
const REFRESH_MS: f64 = 120.0;

#[derive(Properties, PartialEq)]
pub struct SimProps {
    pub file_id: Uuid,
    /// Version number to simulate (pinned by the server).
    pub version: i32,
    pub config: SimConfig,
    /// Names of the model's root Outport blocks, plotted by default.
    pub outports: Vec<String>,
}

/// Signal label without the leading model name.
fn label(name: &str) -> &str {
    name.split_once('/').map_or(name, |(_, rest)| rest)
}

/// Signals to plot for a new data set: the root Outports when any are
/// present, otherwise the first few signals.
fn default_plotted(signals: &[SimulationSignal], outports: &[String]) -> Vec<usize> {
    let outs: Vec<usize> = signals
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let l = label(&s.name);
            outports.iter().any(|o| {
                l.strip_prefix(o.as_str())
                    .is_some_and(|r| r.starts_with(':'))
            })
        })
        .map(|(i, _)| i)
        .take(DEFAULT_PLOTTED)
        .collect();
    if outs.is_empty() {
        (0..signals.len().min(DEFAULT_PLOTTED)).collect()
    } else {
        outs
    }
}

#[derive(Clone, PartialEq)]
enum RunState {
    Idle,
    Connecting,
    Running(Option<Uuid>),
    Finished(SimulationStatus, Option<String>),
    Error(String),
}

impl RunState {
    fn active(&self) -> bool {
        matches!(self, RunState::Connecting | RunState::Running(_))
    }
}

/// Mutable state shared between the component and its async tasks.
#[derive(Default)]
struct Controller {
    /// Identifies the current data set; bumped on every run/load and on unmount.
    generation: u64,
    signals: Vec<SimulationSignal>,
    plotted: Vec<usize>,
    time: Vec<f64>,
    columns: Vec<Vec<f64>>,
    session: Option<PlotSession>,
    sender: Option<Sender<SimulationSocket>>,
    /// Server id of the live run, once known.
    run_id: Option<Uuid>,
    /// Aborts the live run's socket task, dropping its receiver at once.
    task: Option<AbortHandle>,
    last_refresh: f64,
}

impl Controller {
    /// Start a new data set, invalidating every task of the previous one.
    fn reset(&mut self) -> u64 {
        self.generation += 1;
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.signals.clear();
        self.plotted.clear();
        self.time.clear();
        self.columns.clear();
        self.session = None;
        self.sender = None;
        self.run_id = None;
        self.generation
    }

    fn refresh_plot(&self) {
        if let Some(session) = &self.session {
            for (line, &i) in self.plotted.iter().enumerate() {
                if let Some(y) = self.columns.get(i) {
                    let n = y.len().min(self.time.len());
                    let _ = session.set_line_data(0, line, &self.time[..n], &y[..n]);
                }
            }
        }
    }

    /// Build a figure with one line per plotted signal and bind it to the canvas.
    fn bind_plot(&mut self, canvas_id: &str, width_px: f64) -> Result<(), String> {
        self.session = None;
        if self.signals.is_empty() {
            return Ok(());
        }
        let err = |e: wasm_bindgen::JsValue| e.as_string().unwrap_or_else(|| "plot error".into());
        let mut fig = WasmFigure::new((width_px / 100.0).max(4.0), 3.6);
        fig.set_facecolor("#1a1b26").map_err(err)?;
        let ax = fig.add_subplot(1, 1, 1).map_err(err)?;
        fig.oscilloscope(ax).map_err(err)?;
        fig.set_xlabel(ax, "time (s)").map_err(err)?;
        for &i in &self.plotted {
            let y = self.columns.get(i).map(Vec::as_slice).unwrap_or(&[]);
            let n = y.len().min(self.time.len());
            fig.plot(ax, &self.time[..n], &y[..n]).map_err(err)?;
        }
        if !self.plotted.is_empty() {
            let labels = self
                .plotted
                .iter()
                .map(|&i| label(&self.signals[i].name).to_string())
                .collect();
            fig.legend(ax, labels).map_err(err)?;
        }
        self.session = Some(plot::bind(fig, canvas_id).map_err(err)?);
        Ok(())
    }

    fn csv(&self) -> String {
        let mut out = String::from("time");
        for s in &self.signals {
            out.push(',');
            out.push_str(&csv_field(&s.name));
        }
        out.push('\n');
        for (row, t) in self.time.iter().enumerate() {
            out.push_str(&t.to_string());
            for col in &self.columns {
                out.push(',');
                if let Some(v) = col.get(row) {
                    out.push_str(&v.to_string());
                }
            }
            out.push('\n');
        }
        out
    }
}

/// RFC 4180 field: quoted when it contains a delimiter, quote or newline.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn parse_time(s: &Option<String>, default: f64) -> f64 {
    s.as_deref()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// Initial options from the model's solver configuration.
fn defaults(config: &SimConfig) -> SimulationOptions {
    let start = parse_time(&config.start_time, 0.0);
    let stop = parse_time(&config.stop_time, 10.0).max(start + 1e-9);
    let solver = match config.solver.as_deref().map(str::trim) {
        Some("ode1") => Solver::Euler,
        Some(
            "ode2" | "ode3" | "ode4" | "ode5" | "ode8" | "ode14x" | "FixedStepAuto"
            | "FixedStepDiscrete",
        ) => Solver::Rk4,
        _ => Solver::Rk45,
    };
    let step = parse_time(&config.fixed_step, (stop - start) / 1000.0);
    SimulationOptions {
        start,
        stop,
        step: if step > 0.0 {
            step
        } else {
            (stop - start) / 1000.0
        },
        solver,
        ..SimulationOptions::default()
    }
}

fn solver_name(s: Solver) -> &'static str {
    match s {
        Solver::Euler => "euler",
        Solver::Rk4 => "rk4",
        Solver::Rk45 => "rk45",
    }
}

/// Parse `NAME = EXPR` lines; blank lines and `%` comments are skipped.
fn parse_workspace(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let (name, expr) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected NAME = EXPR", i + 1))?;
        out.insert(
            name.trim().to_string(),
            expr.trim().trim_end_matches(';').to_string(),
        );
    }
    Ok(out)
}

/// Stream one run's messages into the controller until it finishes, fails,
/// or the data set is superseded.
async fn stream(
    ctl: Rc<RefCell<Controller>>,
    generation: u64,
    mut rx: ws_bridge::yew_client::Receiver<SimulationSocket>,
    state: UseStateHandle<RunState>,
    rebind: UseReducerHandle<Reload>,
    outports: Vec<String>,
) {
    let current = |ctl: &Rc<RefCell<Controller>>| ctl.borrow().generation == generation;
    let mut terminal = false;
    while let Some(msg) = rx.recv().await {
        if !current(&ctl) {
            return;
        }
        match msg {
            Ok(SimulationServerMsg::SimulationStatus { run }) => match run.status {
                SimulationStatus::Running => {
                    ctl.borrow_mut().run_id = Some(run.id);
                    state.set(RunState::Running(Some(run.id)));
                }
                done => {
                    ctl.borrow().refresh_plot();
                    state.set(RunState::Finished(done, run.error));
                    terminal = true;
                    break;
                }
            },
            Ok(SimulationServerMsg::SimulationStarted { run_id, signals }) => {
                {
                    let mut c = ctl.borrow_mut();
                    c.columns = vec![Vec::new(); signals.len()];
                    c.plotted = default_plotted(&signals, &outports);
                    c.signals = signals;
                    c.run_id = Some(run_id);
                }
                rebind.dispatch(());
                state.set(RunState::Running(Some(run_id)));
            }
            Ok(SimulationServerMsg::SimulationSamples { time, values, .. }) => {
                let mut c = ctl.borrow_mut();
                c.time.extend_from_slice(&time);
                for row in &values {
                    for (col, v) in c.columns.iter_mut().zip(row) {
                        col.push(*v);
                    }
                }
                let now = js_sys::Date::now();
                if now - c.last_refresh > REFRESH_MS {
                    c.last_refresh = now;
                    c.refresh_plot();
                }
            }
            Ok(SimulationServerMsg::Error { message }) => {
                state.set(RunState::Error(message));
                terminal = true;
                break;
            }
            Ok(SimulationServerMsg::Heartbeat) => {}
            Err(e) => {
                state.set(RunState::Error(format!("connection error: {e}")));
                terminal = true;
                break;
            }
        }
    }
    if current(&ctl) {
        ctl.borrow_mut().sender = None;
        if !terminal {
            state.set(RunState::Error(
                "connection closed before the run finished".into(),
            ));
        }
    }
}

#[function_component(SimulationPanel)]
pub fn simulation_panel(props: &SimProps) -> Html {
    let options = use_state(|| defaults(&props.config));
    let workspace = use_state(String::new);
    let state = use_state(|| RunState::Idle);
    let ctl = use_mut_ref(Controller::default);
    // Bumped whenever the plot must be rebuilt (new data set or selection).
    let rebind = use_reload();
    let plot_host = use_node_ref();
    let reload = use_reload();
    let runs = use_fetch((props.file_id, reload.0), |(f, _)| api::simulation_runs(f));
    let canvas_id = format!("sim-plot-{}", props.file_id);

    {
        let (ctl, plot_host, canvas_id) = (ctl.clone(), plot_host.clone(), canvas_id.clone());
        use_effect_with(rebind.0, move |_| {
            let width = plot_host
                .cast::<web_sys::HtmlElement>()
                .map(|e| e.client_width() as f64)
                .unwrap_or(800.0);
            if let Err(e) = ctl.borrow_mut().bind_plot(&canvas_id, width) {
                web_sys::console::error_1(&e.into());
            }
        });
    }

    // On unmount: supersede any running stream and ask the server to cancel.
    {
        let ctl = ctl.clone();
        use_effect_with((), move |_| {
            move || {
                let (sender, run_id) = {
                    let mut c = ctl.borrow_mut();
                    let live = (c.sender.take(), c.run_id.take());
                    c.reset();
                    live
                };
                // Aborting the task dropped the receiver; ask the server to
                // cancel, then drop the sender so the socket closes.
                if let (Some(mut tx), Some(run_id)) = (sender, run_id) {
                    spawn_local(async move {
                        let _ = tx
                            .send(SimulationClientMsg::CancelSimulation { run_id })
                            .await;
                    });
                }
            }
        });
    }

    let run = {
        let (options, workspace, state, ctl, rebind, reload) = (
            options.clone(),
            workspace.clone(),
            state.clone(),
            ctl.clone(),
            rebind.clone(),
            reload.clone(),
        );
        let (file_id, version) = (props.file_id, props.version);
        let outports = props.outports.clone();
        Callback::from(move |_: MouseEvent| {
            let ws_vars = match parse_workspace(&workspace) {
                Ok(v) => v,
                Err(e) => {
                    state.set(RunState::Error(e));
                    return;
                }
            };
            let request = SimulationRequest {
                options: (*options).clone(),
                workspace: ws_vars,
                version: Some(version),
                init_script: None,
            };
            let conn = match ws_bridge::yew_client::connect::<SimulationSocket>() {
                Ok(c) => c,
                Err(e) => {
                    state.set(RunState::Error(format!("cannot connect: {e}")));
                    return;
                }
            };
            let generation = ctl.borrow_mut().reset();
            rebind.dispatch(());
            state.set(RunState::Connecting);
            let (mut tx, rx) = conn.split();
            let (state, ctl, rebind, reload, outports) = (
                state.clone(),
                ctl.clone(),
                rebind.clone(),
                reload.clone(),
                outports.clone(),
            );
            let (abort, registration) = AbortHandle::new_pair();
            ctl.borrow_mut().task = Some(abort);
            let task = async move {
                if let Err(e) = tx
                    .send(SimulationClientMsg::Simulate { file_id, request })
                    .await
                {
                    state.set(RunState::Error(format!("send failed: {e}")));
                    return;
                }
                if ctl.borrow().generation != generation {
                    return;
                }
                ctl.borrow_mut().sender = Some(tx);
                stream(ctl, generation, rx, state, rebind, outports).await;
                reload.dispatch(());
            };
            spawn_local(async move {
                let _ = Abortable::new(task, registration).await;
            });
        })
    };

    let cancel = {
        let (ctl, state) = (ctl.clone(), state.clone());
        Callback::from(move |_: MouseEvent| {
            let RunState::Running(Some(run_id)) = &*state else {
                return;
            };
            let run_id = *run_id;
            let ctl = ctl.clone();
            spawn_local(async move {
                let (tx, generation) = {
                    let mut c = ctl.borrow_mut();
                    (c.sender.take(), c.generation)
                };
                if let Some(mut tx) = tx {
                    let _ = tx
                        .send(SimulationClientMsg::CancelSimulation { run_id })
                        .await;
                    // Only restore the sender if its run is still current.
                    let mut c = ctl.borrow_mut();
                    if c.generation == generation {
                        c.sender = Some(tx);
                    }
                }
            });
        })
    };

    let load_run = {
        let (state, ctl, rebind) = (state.clone(), ctl.clone(), rebind.clone());
        let outports = props.outports.clone();
        move |run: SimulationRun| {
            let (state, ctl, rebind, outports) =
                (state.clone(), ctl.clone(), rebind.clone(), outports.clone());
            Callback::from(move |_: MouseEvent| {
                if state.active() {
                    return;
                }
                let generation = ctl.borrow_mut().reset();
                rebind.dispatch(());
                let (state, ctl, rebind, outports) =
                    (state.clone(), ctl.clone(), rebind.clone(), outports.clone());
                let id = run.id;
                spawn_local(async move {
                    let result = api::simulation_result(id).await;
                    if ctl.borrow().generation != generation {
                        return;
                    }
                    match result {
                        Ok(result) => {
                            if let Some(trace) = result.trace {
                                let mut c = ctl.borrow_mut();
                                c.signals = trace
                                    .signals
                                    .keys()
                                    .map(|k| SimulationSignal {
                                        id: k.clone(),
                                        name: k.clone(),
                                    })
                                    .collect();
                                c.plotted = default_plotted(&c.signals, &outports);
                                c.time = trace.time;
                                c.columns = trace.signals.into_values().collect();
                            }
                            rebind.dispatch(());
                            state.set(RunState::Finished(result.run.status, result.run.error));
                        }
                        Err(e) => state.set(RunState::Error(e.to_string())),
                    }
                });
            })
        }
    };

    let set_num = |field: fn(&mut SimulationOptions, f64)| {
        let options = options.clone();
        Callback::from(move |e: InputEvent| {
            if let Ok(v) = e
                .target_unchecked_into::<HtmlInputElement>()
                .value()
                .parse::<f64>()
            {
                let mut o = (*options).clone();
                field(&mut o, v);
                options.set(o);
            }
        })
    };
    let set_solver = {
        let options = options.clone();
        Callback::from(move |e: Event| {
            let mut o = (*options).clone();
            o.solver = match e
                .target_unchecked_into::<HtmlSelectElement>()
                .value()
                .as_str()
            {
                "euler" => Solver::Euler,
                "rk4" => Solver::Rk4,
                _ => Solver::Rk45,
            };
            options.set(o);
        })
    };
    let set_workspace = {
        let workspace = workspace.clone();
        Callback::from(move |e: InputEvent| {
            workspace.set(e.target_unchecked_into::<HtmlTextAreaElement>().value())
        })
    };
    let toggle_signal = |i: usize| {
        let (ctl, rebind) = (ctl.clone(), rebind.clone());
        Callback::from(move |_: Event| {
            {
                let mut c = ctl.borrow_mut();
                if let Some(pos) = c.plotted.iter().position(|&x| x == i) {
                    c.plotted.remove(pos);
                } else {
                    c.plotted.push(i);
                    c.plotted.sort_unstable();
                }
            }
            rebind.dispatch(());
        })
    };

    let running = state.active();
    let c = ctl.borrow();
    let samples = c.time.len();
    let status = match &*state {
        RunState::Idle => html! {},
        RunState::Connecting => html! { <span class="muted">{ "Connecting…" }</span> },
        RunState::Running(_) => {
            html! { <span class="running">{ format!("Running… {samples} samples") }</span> }
        }
        RunState::Finished(SimulationStatus::Completed, _) => {
            html! { <span class="ok">{ format!("Completed · {samples} samples") }</span> }
        }
        RunState::Finished(s, err) => html! {
            <span class="error">{ format!("{s:?}{}", err.as_ref().map(|e| format!(": {e}")).unwrap_or_default()) }</span>
        },
        RunState::Error(e) => html! { <span class="error">{ e }</span> },
    };
    let csv_href = (!c.signals.is_empty() && !running).then(|| {
        format!(
            "data:text/csv;charset=utf-8,{}",
            String::from(js_sys::encode_uri_component(&c.csv()))
        )
    });
    let o = &*options;

    html! {
        <div class="sim-panel">
            <div class="sim-form">
                <label>{ "Start" }<input type="number" step="any" value={o.start.to_string()} oninput={set_num(|o, v| o.start = v)} /></label>
                <label>{ "Stop" }<input type="number" step="any" value={o.stop.to_string()} oninput={set_num(|o, v| o.stop = v)} /></label>
                <label>{ "Output step" }<input type="number" step="any" value={o.step.to_string()} oninput={set_num(|o, v| o.step = v)} /></label>
                <label>{ "Solver" }
                    <select onchange={set_solver}>
                        { for [Solver::Rk45, Solver::Rk4, Solver::Euler].into_iter().map(|s| html! {
                            <option value={solver_name(s)} selected={s == o.solver}>{ solver_name(s) }</option>
                        }) }
                    </select>
                </label>
                if o.solver == Solver::Rk45 {
                    <label>{ "Rel tol" }<input type="number" step="any" value={o.relative_tolerance.to_string()} oninput={set_num(|o, v| o.relative_tolerance = v)} /></label>
                    <label>{ "Abs tol" }<input type="number" step="any" value={o.absolute_tolerance.to_string()} oninput={set_num(|o, v| o.absolute_tolerance = v)} /></label>
                }
                <label class="grow">{ "Workspace (NAME = EXPR per line)" }
                    <textarea rows="2" value={(*workspace).clone()} oninput={set_workspace} placeholder="K = 2\nw = 2*pi*5" />
                </label>
                <div class="sim-actions">
                    if running {
                        <button onclick={cancel}>{ "Cancel" }</button>
                    } else {
                        <button class="primary" onclick={run}>{ format!("Run v{}", props.version) }</button>
                    }
                    { status }
                    if let Some(href) = csv_href {
                        <a class="button" href={href} download="trace.csv">{ "CSV" }</a>
                    }
                </div>
            </div>
            <div class="sim-body">
                <div class="plot-host" ref={plot_host}>
                    <canvas id={canvas_id.clone()} />
                </div>
                <aside class="signal-list">
                    <h4>{ "Signals" }</h4>
                    { for c.signals.iter().enumerate().map(|(i, s)| html! {
                        <label class="signal">
                            <input type="checkbox" checked={c.plotted.contains(&i)} onchange={toggle_signal(i)} />
                            { label(&s.name) }
                        </label>
                    }) }
                    <h4>{ "Previous runs" }</h4>
                    { view(&runs, |rs: &Vec<SimulationRun>| html! {
                        <ul class="plain runs">
                            { for rs.iter().map(|r| html! {
                                <li>
                                    <a class={classes!(running.then_some("disabled"))} onclick={load_run(r.clone())}>
                                        { format!("v{} · {:?}", r.file_version, r.status) }
                                    </a>
                                    <div class="muted">{ r.created_at.format("%Y-%m-%d %H:%M:%S").to_string() }</div>
                                </li>
                            }) }
                        </ul>
                    }) }
                </aside>
            </div>
        </div>
    }
}
