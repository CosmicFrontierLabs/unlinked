use super::*;
use unlinked_model::{Block, Model, PortKind};

fn parameter(
    block: &Block,
    key: &str,
    default: &str,
    ws: &BTreeMap<String, f64>,
) -> Result<f64, Error> {
    let source = block.param(key).unwrap_or(default);
    let value = unlinked_matlab::eval_expr(source, ws)
        .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
    if !value.is_finite() {
        return Err(block_error(&block.id.0, format!("{key} must be finite")));
    }
    Ok(value)
}
fn require(block: &Block, key: &str, allowed: &[&str]) -> Result<(), Error> {
    if let Some(value) = block.param(key) {
        if !allowed.contains(&value) {
            return Err(block_error(
                &block.id.0,
                format!("unsupported {key}={value}"),
            ));
        }
    }
    Ok(())
}

/// Compile the root system. Library links, masked blocks, subsystems, non-scalar
/// signals, and unimplemented block semantics produce diagnostics.
/// Workspace expressions are evaluated as scalar math, never as MATLAB scripts.
pub fn compile(model: &Model, options: &Options) -> Result<Graph, Error> {
    let mut ws = BTreeMap::new();
    for key in model.workspace.keys() {
        if ["pi", "Inf", "NaN", "true", "false"].contains(&key.as_str()) {
            return Err(Error::Parameter(
                key.clone(),
                "overriding built-in constants in model workspace is unsupported".into(),
            ));
        }
    }
    let mut pending = model.workspace.clone();
    while !pending.is_empty() {
        let before = pending.len();
        let mut errors = vec![];
        for (key, source) in pending.clone() {
            match unlinked_matlab::eval_expr(&source, &ws) {
                Ok(value) if value.is_finite() => {
                    ws.insert(key.clone(), value);
                    pending.remove(&key);
                }
                Ok(_) => errors.push((key, "non-finite expression".into())),
                Err(e) => errors.push((key, e.to_string())),
            }
        }
        if before == pending.len() {
            let (key, error) = errors.remove(0);
            return Err(Error::Parameter(key, error));
        }
    }
    let mut graph = Graph::default();
    for block in &model.root.blocks {
        let id = &block.id.0;
        if block.subsystem.is_some() || block.mask.is_some() || block.library_source.is_some() {
            return Err(block_error(
                id,
                "subsystems, masks and library links require explicit lowering before simulation",
            ));
        }
        if block.ports.outputs > 1
            || block.ports.enable
                + block.ports.trigger
                + block.ports.state
                + block.ports.lconn
                + block.ports.rconn
                + block.ports.ifaction
                + block.ports.reset
                > 0
        {
            return Err(block_error(
                id,
                "only one scalar output and ordinary input ports are supported",
            ));
        }
        require(
            block,
            "OutDataTypeStr",
            &[
                "Inherit: Inherit via internal rule",
                "Inherit: Inherit via back propagation",
                "Inherit: Same as input",
                "double",
            ],
        )?;
        require(block, "SignalType", &["auto", "real"])?;
        require(block, "SaturateOnIntegerOverflow", &["off"])?;
        let p = |key, default| parameter(block, key, default, &ws);
        if let Some(sample) = block.param("SampleTime") {
            let value = unlinked_matlab::eval_expr(sample, &ws)
                .map_err(|e| block_error(id, format!("SampleTime: {e}")))?;
            let acceptable = if block.block_type == "UnitDelay" {
                value == -1.0 || (value - options.step).abs() <= 1e-12 * options.step.abs()
            } else {
                value == -1.0
                    || value == 0.0
                    || (block.block_type == "Constant" && value == f64::INFINITY)
            };
            if !acceptable {
                return Err(block_error(id,"multirate/sample-time semantics are unsupported; UnitDelay sample time must equal simulation step"));
            }
        }
        let kind = match block.block_type.as_str() {
            "Constant" => Kind::Constant {
                value: p("Value", "1")?,
            },
            "Ground" => Kind::Constant { value: 0.0 },
            "Clock" => Kind::Clock,
            "Step" => {
                let time = p("Time", "1")?;
                let ticks = (time - options.start) / options.step;
                if time > options.start
                    && time < options.stop
                    && (ticks - ticks.round()).abs() > 1e-9
                {
                    return Err(block_error(
                        id,
                        "Step transition must align with fixed-step grid",
                    ));
                }
                Kind::Step {
                    time,
                    before: p("Before", "0")?,
                    after: p("After", "1")?,
                }
            }
            "Sin" => {
                require(block, "SineType", &["Time based"])?;
                require(block, "TimeSource", &["Use simulation time"])?;
                Kind::Sine {
                    amplitude: p("Amplitude", "1")?,
                    frequency: p("Frequency", "1")?,
                    phase: p("Phase", "0")?,
                    bias: p("Bias", "0")?,
                }
            }
            "Gain" => {
                require(block, "Multiplication", &["Element-wise(K.*u)"])?;
                Kind::Gain {
                    gain: p("Gain", "1")?,
                }
            }
            "Bias" => Kind::Bias {
                bias: p("Bias", "0")?,
            },
            "Sum" | "Add" => {
                let raw = block.param("Inputs").unwrap_or("++");
                let signs = if let Ok(count) = raw.parse::<usize>() {
                    if count == 0 || count > 1024 {
                        return Err(block_error(id, "invalid Sum input count"));
                    }
                    vec![1.0; count]
                } else {
                    raw.chars()
                        .filter(|c| !c.is_whitespace() && *c != '|')
                        .map(|c| match c {
                            '+' => Ok(1.0),
                            '-' => Ok(-1.0),
                            _ => Err(block_error(id, "invalid Sum signs")),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                if signs.is_empty() {
                    return Err(block_error(id, "empty Sum inputs"));
                }
                Kind::Sum { signs }
            }
            "Product" => {
                require(block, "Multiplication", &["Element-wise(.*)"])?;
                let raw = block.param("Inputs").unwrap_or("2");
                let divide = if let Ok(count) = raw.parse::<usize>() {
                    if count == 0 || count > 1024 {
                        return Err(block_error(id, "invalid Product input count"));
                    }
                    vec![false; count]
                } else {
                    raw.chars()
                        .map(|c| match c {
                            '*' => Ok(false),
                            '/' => Ok(true),
                            _ => Err(block_error(id, "invalid Product inputs")),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                if divide.is_empty() {
                    return Err(block_error(id, "empty Product inputs"));
                }
                Kind::Product { divide }
            }
            "Saturate" | "Saturation" => Kind::Saturation {
                lower: p("LowerLimit", "-0.5")?,
                upper: p("UpperLimit", "0.5")?,
            },
            "Integrator" => {
                require(block, "ExternalReset", &["none"])?;
                require(block, "InitialConditionSource", &["internal"])?;
                require(block, "LimitOutput", &["off"])?;
                require(block, "WrapState", &["off"])?;
                require(block, "ShowStatePort", &["off"])?;
                Kind::Integrator {
                    initial: p("InitialCondition", "0")?,
                }
            }
            "UnitDelay" => Kind::UnitDelay {
                initial: p("InitialCondition", "0")?,
            },
            "Abs" => Kind::Abs,
            "Trigonometry" => Kind::Unary {
                operation: block.param("Operator").unwrap_or("sin").into(),
            },
            "Math" => Kind::Unary {
                operation: block.param("Operator").unwrap_or("exp").into(),
            },
            "Scope" | "Display" | "Outport" | "Terminator" | "ToWorkspace" => {
                if block.ports.inputs > 1 {
                    return Err(block_error(id, "multi-input sinks unsupported"));
                }
                Kind::Sink
            }
            other => return Err(block_error(id, format!("unsupported block type {other}"))),
        };
        graph.nodes.push(Node {
            id: id.clone(),
            name: block.name.clone(),
            kind,
        });
    }
    for connection in model.root.connections() {
        if connection.src.port.kind != PortKind::Out
            || connection.src.port.index != 1
            || connection.dst.port.kind != PortKind::In
            || connection.dst.port.index == 0
        {
            return Err(Error::Connection(
                "only ordinary scalar ports are supported".into(),
            ));
        }
        graph.wires.push(Wire {
            source: connection.src.block.0,
            target: connection.dst.block.0,
            input: (connection.dst.port.index - 1) as usize,
        });
    }
    Compiled::new(&graph)?;
    Ok(graph)
}

/// Explicit options override imported solver settings. The caller must display
/// the selected solver/step; no claim of matching arbitrary Simulink solvers is made.
pub fn simulate_model(model: &Model, options: &Options) -> Result<Trace, Error> {
    simulate(&compile(model, options)?, options)
}
