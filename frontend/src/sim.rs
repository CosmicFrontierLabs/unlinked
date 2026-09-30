//! Run simulations of a stored model version and plot the streamed traces
//! with rizzma (oscilloscope-styled, zoom/pan in the canvas).

use crate::api;
use crate::fetch::{use_fetch, use_reload, view};
use rizzma::wasm::{WasmFigure, WasmSession};
use shared::{
    SimulationClientMsg, SimulationOptions, SimulationRequest, SimulationRun, SimulationServerMsg,
    SimulationSignal, SimulationSocket, SimulationStatus, Solver,
};
use std::collections::BTreeMap;
use unlinked_model::SimConfig;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Event, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement, InputEvent};
use yew::prelude::*;

/// Signals plotted by default when a run starts.
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

/// Signals to plot when a run starts: the root Outports when any are
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

/// Streamed samples, one column per signal.
#[derive(Default)]
struct Buffers {
    time: Vec<f64>,
    columns: Vec<Vec<f64>>,
}

#[derive(Clone, PartialEq)]
enum RunState {
    Idle,
    Connecting,
    Running(Option<Uuid>),
    Finished(SimulationStatus, Option<String>),
    Error(String),
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
    let solver_name = config
        .solver
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let solver = if solver_name.contains("ode1") || solver_name.contains("euler") {
        Solver::Euler
    } else if solver_name.contains("ode4") {
        Solver::Rk4
    } else {
        Solver::Rk45
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

/// Build a figure with one line per plotted signal and bind it to the canvas.
fn bind_plot(
    canvas_id: &str,
    width_px: f64,
    signals: &[SimulationSignal],
    plotted: &[usize],
    buffers: &Buffers,
) -> Result<WasmSession, String> {
    let err = |e: wasm_bindgen::JsValue| e.as_string().unwrap_or_else(|| "plot error".into());
    let mut fig = WasmFigure::new((width_px / 100.0).max(4.0), 3.6);
    fig.set_facecolor("#1a1b26").map_err(err)?;
    let ax = fig.add_subplot(1, 1, 1).map_err(err)?;
    fig.oscilloscope(ax).map_err(err)?;
    fig.set_xlabel(ax, "time (s)").map_err(err)?;
    for &i in plotted {
        let y = buffers.columns.get(i).map(Vec::as_slice).unwrap_or(&[]);
        let n = y.len().min(buffers.time.len());
        fig.plot(ax, &buffers.time[..n], &y[..n]).map_err(err)?;
    }
    if !plotted.is_empty() {
        fig.legend(
            ax,
            plotted
                .iter()
                .map(|&i| label(&signals[i].name).to_string())
                .collect(),
        )
        .map_err(err)?;
    }
    fig.bind(canvas_id).map_err(err)
}

fn refresh_plot(session: &WasmSession, plotted: &[usize], buffers: &Buffers) {
    for (line, &i) in plotted.iter().enumerate() {
        if let Some(y) = buffers.columns.get(i) {
            let n = y.len().min(buffers.time.len());
            let _ = session.set_line_data(0, line, &buffers.time[..n], &y[..n]);
        }
    }
}

fn csv(signals: &[SimulationSignal], buffers: &Buffers) -> String {
    let mut out = String::from("time");
    for s in signals {
        out.push(',');
        out.push_str(&s.name.replace([',', '\n', '"'], " "));
    }
    out.push('\n');
    for (row, t) in buffers.time.iter().enumerate() {
        out.push_str(&t.to_string());
        for col in &buffers.columns {
            out.push(',');
            if let Some(v) = col.get(row) {
                out.push_str(&v.to_string());
            }
        }
        out.push('\n');
    }
    out
}

#[function_component(SimulationPanel)]
pub fn simulation_panel(props: &SimProps) -> Html {
    let options = use_state(|| defaults(&props.config));
    let workspace = use_state(String::new);
    let state = use_state(|| RunState::Idle);
    let signals = use_state(Vec::<SimulationSignal>::new);
    let plotted = use_state(Vec::<usize>::new);
    let buffers = use_mut_ref(Buffers::default);
    let session = use_mut_ref(|| None::<WasmSession>);
    let sender = use_mut_ref(|| None::<ws_bridge::yew_client::Sender<SimulationSocket>>);
    let last_refresh = use_mut_ref(|| 0.0f64);
    let plot_host = use_node_ref();
    let reload = use_reload();
    let runs = use_fetch((props.file_id, reload.0), |(f, _)| api::simulation_runs(f));
    let canvas_id = format!("sim-plot-{}", props.file_id);

    // Rebind the figure whenever the plotted set changes.
    {
        let (session, buffers, signals, plot_host, canvas_id) = (
            session.clone(),
            buffers.clone(),
            signals.clone(),
            plot_host.clone(),
            canvas_id.clone(),
        );
        use_effect_with((*plotted).clone(), move |plotted| {
            let width = plot_host
                .cast::<web_sys::HtmlElement>()
                .map(|e| e.client_width() as f64)
                .unwrap_or(800.0);
            *session.borrow_mut() = None;
            if !signals.is_empty() {
                match bind_plot(&canvas_id, width, &signals, plotted, &buffers.borrow()) {
                    Ok(s) => *session.borrow_mut() = Some(s),
                    Err(e) => gloo_console_log(&e),
                }
            }
        });
    }

    let run = {
        let (
            options,
            workspace,
            state,
            signals,
            plotted,
            buffers,
            session,
            sender,
            last_refresh,
            reload,
        ) = (
            options.clone(),
            workspace.clone(),
            state.clone(),
            signals.clone(),
            plotted.clone(),
            buffers.clone(),
            session.clone(),
            sender.clone(),
            last_refresh.clone(),
            reload.clone(),
        );
        let (file_id, version) = (props.file_id, props.version);
        let outports = props.outports.clone();
        Callback::from(move |_: MouseEvent| {
            let outports = outports.clone();
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
            };
            let conn = match ws_bridge::yew_client::connect::<SimulationSocket>() {
                Ok(c) => c,
                Err(e) => {
                    state.set(RunState::Error(format!("cannot connect: {e}")));
                    return;
                }
            };
            *buffers.borrow_mut() = Buffers::default();
            *session.borrow_mut() = None;
            signals.set(Vec::new());
            plotted.set(Vec::new());
            state.set(RunState::Connecting);
            let (mut tx, mut rx) = conn.split();
            let (state, signals, plotted, buffers, session, sender, last_refresh, reload) = (
                state.clone(),
                signals.clone(),
                plotted.clone(),
                buffers.clone(),
                session.clone(),
                sender.clone(),
                last_refresh.clone(),
                reload.clone(),
            );
            spawn_local(async move {
                if let Err(e) = tx
                    .send(SimulationClientMsg::Simulate { file_id, request })
                    .await
                {
                    state.set(RunState::Error(format!("send failed: {e}")));
                    return;
                }
                *sender.borrow_mut() = Some(tx);
                // Plotted indices as known to this task (state handles are snapshots).
                let mut shown: Vec<usize> = Vec::new();
                while let Some(msg) = rx.recv().await {
                    match msg {
                        Ok(SimulationServerMsg::SimulationStatus { run }) => match run.status {
                            SimulationStatus::Running => state.set(RunState::Running(Some(run.id))),
                            done => {
                                if let Some(s) = session.borrow().as_ref() {
                                    refresh_plot(s, &shown, &buffers.borrow());
                                }
                                state.set(RunState::Finished(done, run.error));
                                break;
                            }
                        },
                        Ok(SimulationServerMsg::SimulationStarted {
                            run_id,
                            signals: sigs,
                        }) => {
                            buffers.borrow_mut().columns = vec![Vec::new(); sigs.len()];
                            shown = default_plotted(&sigs, &outports);
                            signals.set(sigs);
                            plotted.set(shown.clone());
                            state.set(RunState::Running(Some(run_id)));
                        }
                        Ok(SimulationServerMsg::SimulationSamples { time, values, .. }) => {
                            {
                                let mut b = buffers.borrow_mut();
                                b.time.extend_from_slice(&time);
                                for row in &values {
                                    for (col, v) in b.columns.iter_mut().zip(row) {
                                        col.push(*v);
                                    }
                                }
                            }
                            let now = js_sys::Date::now();
                            if now - *last_refresh.borrow() > REFRESH_MS {
                                *last_refresh.borrow_mut() = now;
                                if let Some(s) = session.borrow().as_ref() {
                                    refresh_plot(s, &shown, &buffers.borrow());
                                }
                            }
                        }
                        Ok(SimulationServerMsg::Error { message }) => {
                            state.set(RunState::Error(message));
                            break;
                        }
                        Ok(SimulationServerMsg::Heartbeat) => {}
                        Err(e) => {
                            state.set(RunState::Error(format!("connection error: {e}")));
                            break;
                        }
                    }
                }
                *sender.borrow_mut() = None;
                reload.dispatch(());
            });
        })
    };

    let cancel = {
        let (sender, state) = (sender.clone(), state.clone());
        Callback::from(move |_: MouseEvent| {
            let RunState::Running(Some(run_id)) = &*state else {
                return;
            };
            let run_id = *run_id;
            let sender = sender.clone();
            spawn_local(async move {
                let tx = sender.borrow_mut().take();
                if let Some(mut tx) = tx {
                    let _ = tx
                        .send(SimulationClientMsg::CancelSimulation { run_id })
                        .await;
                    *sender.borrow_mut() = Some(tx);
                }
            });
        })
    };

    let load_run = {
        let (state, signals, plotted, buffers, session) = (
            state.clone(),
            signals.clone(),
            plotted.clone(),
            buffers.clone(),
            session.clone(),
        );
        let outports = props.outports.clone();
        move |run: SimulationRun| {
            let (state, signals, plotted, buffers, session) = (
                state.clone(),
                signals.clone(),
                plotted.clone(),
                buffers.clone(),
                session.clone(),
            );
            let outports = outports.clone();
            Callback::from(move |_: MouseEvent| {
                let (state, signals, plotted, buffers, session) = (
                    state.clone(),
                    signals.clone(),
                    plotted.clone(),
                    buffers.clone(),
                    session.clone(),
                );
                let outports = outports.clone();
                let id = run.id;
                spawn_local(async move {
                    match api::simulation_result(id).await {
                        Ok(result) => {
                            let Some(trace) = result.trace else {
                                state.set(RunState::Finished(result.run.status, result.run.error));
                                return;
                            };
                            let sigs: Vec<SimulationSignal> = trace
                                .signals
                                .keys()
                                .map(|k| SimulationSignal {
                                    id: k.clone(),
                                    name: k.clone(),
                                })
                                .collect();
                            *session.borrow_mut() = None;
                            *buffers.borrow_mut() = Buffers {
                                time: trace.time,
                                columns: trace.signals.into_values().collect(),
                            };
                            plotted.set(default_plotted(&sigs, &outports));
                            signals.set(sigs);
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
        let plotted = plotted.clone();
        Callback::from(move |_: Event| {
            let mut p = (*plotted).clone();
            if let Some(pos) = p.iter().position(|&x| x == i) {
                p.remove(pos);
            } else {
                p.push(i);
                p.sort_unstable();
            }
            plotted.set(p);
        })
    };

    let running = matches!(*state, RunState::Connecting | RunState::Running(_));
    let status = match &*state {
        RunState::Idle => html! {},
        RunState::Connecting => html! { <span class="muted">{ "Connecting…" }</span> },
        RunState::Running(_) => {
            html! { <span class="running">{ format!("Running… {} samples", buffers.borrow().time.len()) }</span> }
        }
        RunState::Finished(SimulationStatus::Completed, _) => {
            html! { <span class="ok">{ format!("Completed · {} samples", buffers.borrow().time.len()) }</span> }
        }
        RunState::Finished(s, err) => html! {
            <span class="error">{ format!("{s:?}{}", err.as_ref().map(|e| format!(": {e}")).unwrap_or_default()) }</span>
        },
        RunState::Error(e) => html! { <span class="error">{ e }</span> },
    };
    let csv_href = (!signals.is_empty() && !running).then(|| {
        format!(
            "data:text/csv;charset=utf-8,{}",
            String::from(js_sys::encode_uri_component(&csv(
                &signals,
                &buffers.borrow()
            )))
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
                    { for signals.iter().enumerate().map(|(i, s)| html! {
                        <label class="signal">
                            <input type="checkbox" checked={plotted.contains(&i)} onchange={toggle_signal(i)} />
                            { label(&s.name) }
                        </label>
                    }) }
                    <h4>{ "Previous runs" }</h4>
                    { view(&runs, |rs: &Vec<SimulationRun>| html! {
                        <ul class="plain runs">
                            { for rs.iter().map(|r| html! {
                                <li>
                                    <a onclick={load_run(r.clone())}>
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

fn gloo_console_log(msg: &str) {
    web_sys::console::error_1(&msg.into());
}
